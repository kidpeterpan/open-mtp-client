use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use mtp_rs::mtp::{ByteRange, DateTime, ObjectHandle, ObjectInfo, Storage};
use tokio::io::AsyncWriteExt;

use super::mapper;
use super::relay::{Nowhere, Onward};
use crate::dto::{
    Cancel, Conflict, Direction, DownloadRequest, Error, FileFailure, FileOutcome, FileReport,
    Kept, Original, Paste, REPORT_MOST, Removal, TransferProgress, TransferSummary, Why,
};
use crate::service::failures::{is_about_one_object, whole_batch_failure};
use crate::service::{device, explorer, local, names};

#[cfg(test)]
thread_local! {
    /// How many files this test's copies forced to the disk: a test runs on
    /// one thread, so it counts only its own.
    pub(super) static SYNCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Bytes per read. Small enough for smooth progress and quick cancel,
/// large enough that USB round trips don't dominate.
pub(super) const WINDOW: u32 = 2 * 1024 * 1024;

/// How long a transfer waits on a phone that has gone quiet before giving up.
/// A wedged phone (Pixels do this) can stop answering without ever reporting an
/// error, and a copy would hang on it for ever. A window of data takes
/// seconds at the slowest, so this is generous.
pub(super) const STALL_LIMIT: Duration = Duration::from_secs(90);

/// Copies between the phone and the Mac.
pub struct Service {
    pub(super) device: Arc<device::Service>,
    /// What every request to the phone outside a copy's own waits is given
    /// (planning, and removing originals), counted on the device's clock.
    pub(super) limits: explorer::Limits,
    pub(super) explorer: Arc<explorer::Service>,
    pub(super) stall_limit: Duration,
    /// Puts Mac items in the Trash, for a move to the phone. Never set by
    /// `new`: a test must choose a recorder, and the app the real Trash
    /// (`with_trash`).
    pub(super) trash: Option<super::to_phone::Trash>,
    /// What was imported from each phone before (`with_imports`); without
    /// one, an import goes by what is in the destination only.
    pub(super) imports: Option<super::import::ImportRecord>,
    /// The phone side of the last mirror preview, which the mirror run right
    /// after it can use instead of reading every file again.
    pub(super) mirror_reading: tokio::sync::Mutex<Option<super::mirror::Reading>>,
    /// Where copies fetched to be looked at are kept (`with_preview_cache`).
    pub(super) preview_cache: Option<PathBuf>,
    /// What the last look at a storage for freeing space found, which
    /// removing from it goes by.
    pub(super) space_scan: tokio::sync::Mutex<Option<super::space::SpaceScan>>,
    /// Where working copies for editing on the Mac are kept
    /// (`with_editing`); without one, editing is refused.
    pub(super) editing: Option<PathBuf>,
    /// The free space of a Mac folder's disk, checked before a transfer
    /// writes there (`with_room` in tests).
    pub(super) room: super::room::RoomOf,
}

/// What a download does about one file, decided when the copy is planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum How {
    /// Copy it from the start.
    Fresh,
    /// Carry on from a part file an earlier attempt left, `offset` bytes long.
    Resume { part: PathBuf, offset: u64 },
    /// The same file (size and date) is already at the destination.
    AlreadyThere,
}

/// One thing to create at the destination, in order.
pub(super) enum Step {
    Folder(PathBuf),
    File {
        handle: ObjectHandle,
        target: PathBuf,
        /// Shown in progress, relative to the destination.
        label: String,
        size: u64,
        modified: Option<DateTime>,
        how: How,
    },
}

impl Step {
    /// The bytes still to move for this step.
    pub(super) fn bytes_to_copy(&self) -> u64 {
        match self {
            Step::File {
                how: How::Fresh,
                size,
                ..
            } => *size,
            Step::File {
                how: How::Resume { offset, .. },
                size,
                ..
            } => size.saturating_sub(*offset),
            Step::File {
                how: How::AlreadyThere,
                ..
            }
            | Step::Folder(_) => 0,
        }
    }
}

/// What a run of a plan's steps needs besides the plan.
pub(super) struct Run<'a, O> {
    pub(super) storage: &'a Storage,
    pub(super) partial_reads: bool,
    /// The folder the plan's targets are under.
    pub(super) destination: &'a Path,
    pub(super) moving: bool,
    pub(super) started: Instant,
    pub(super) cancel: &'a Cancel,
    pub(super) onward: Option<&'a mut O>,
}

/// How a run of a plan's steps went.
pub(super) struct Executed {
    pub(super) state: TransferProgress,
    /// How each step went, one per step.
    pub(super) outcomes: Vec<Outcome>,
    pub(super) already_there: usize,
    pub(super) resumed: usize,
    pub(super) not_copied: usize,
    pub(super) failures: Vec<FileFailure>,
}

pub(super) struct Plan {
    pub(super) steps: Vec<Step>,
    /// Where each step's original is on the phone, one per step, for a move
    /// to remove it afterwards.
    pub(super) origins: Vec<Origin>,
    /// The names of the selected items, as `Origin::top` counts them.
    pub(super) items: Vec<String>,
    /// A folder was met twice and not walked again (a cycle in the phone's
    /// tree): not everything under it was seen, so a move never removes a
    /// folder.
    pub(super) unwalked: bool,
    pub(super) renamed: usize,
    pub(super) first_item: Option<String>,
    /// For a step staged to replace a Mac file (a download answered Replace
    /// or Replace older), that file; none for the rest. Empty when nothing
    /// is replaced.
    pub(super) replaces: Vec<Option<PathBuf>>,
    /// Items whose name was taken and that the answer left alone, as
    /// labels relative to the destination.
    pub(super) skipped: Vec<String>,
}

/// The hidden folder in a download's destination where copies meant to
/// replace a Mac file wait until they are whole.
const DOWNLOAD_STAGING: &str = ".open-mtp-client-download";

/// An object on the phone that a step copies, as a move needs to find it again.
#[derive(Debug, Clone)]
pub(super) struct Origin {
    handle: ObjectHandle,
    /// The folder it is in, relative to the folder the items were selected
    /// in: empty for the selected items themselves.
    parent: String,
    /// Its name on the phone (which is not always its name on the Mac).
    name: String,
    size: u64,
    modified: Option<DateTime>,
    is_folder: bool,
    /// Which selected item it is, or is inside.
    pub(super) top: usize,
    /// It is a selected item, not something inside one.
    is_top: bool,
}

impl Origin {
    /// A file on the phone, in no selection: what an import may remove.
    pub(super) fn file(
        handle: ObjectHandle,
        name: String,
        size: u64,
        modified: Option<DateTime>,
    ) -> Self {
        Self {
            handle,
            parent: String::new(),
            name,
            size,
            modified,
            is_folder: false,
            top: 0,
            is_top: false,
        }
    }

    pub(super) fn handle(&self) -> ObjectHandle {
        self.handle
    }

    pub(super) fn size(&self) -> u64 {
        self.size
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// It is a selected item, not something inside one.
    pub(super) fn is_top(&self) -> bool {
        self.is_top
    }
}

#[cfg(test)]
impl Origin {
    /// An origin that belongs to the selected item `top`, for tests of what
    /// is decided by item.
    pub(super) fn for_test(top: usize) -> Self {
        Self {
            handle: ObjectHandle(1),
            parent: String::new(),
            name: String::new(),
            size: 0,
            modified: None,
            is_folder: false,
            top,
            is_top: false,
        }
    }
}

/// How far a step got, as far as a move is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Did not copy, or has not been tried: its original stays, and a file
    /// that failed is already in the summary's failures.
    NotCopied,
    /// Copied (or already there) and checked: exactly the size the phone
    /// reported.
    Verified,
    /// Copied, but there is no way to check it: the phone gave no size, or
    /// what arrived is not the size it gave.
    Unchecked,
}

impl Outcome {
    /// A file found at the destination with the phone's size and date: as good
    /// as a copy just made, unless the phone gave no size and so there is
    /// nothing the match means.
    pub(super) fn of_existing(reported: u64) -> Self {
        if size_is_known(reported) {
            Outcome::Verified
        } else {
            Outcome::Unchecked
        }
    }
}

/// A download that has run: what it copied, and what a move needs to go on.
pub(super) struct Downloaded {
    pub(super) summary: TransferSummary,
    pub(super) plan: Plan,
    pub(super) outcomes: Vec<Outcome>,
    pub(super) storage: Storage,
}

/// What copying one file came to.
struct Copied {
    /// It carried on from an earlier attempt's part file.
    resumed: bool,
    /// What is on the Mac is exactly the size the phone reported, and all of
    /// it arrived.
    verified: bool,
}

/// Why a copy stopped, before it becomes a `dto::Error`.
#[derive(Debug)]
pub(super) enum Failure {
    Device(mtp_rs::Error),
    Mac(std::io::Error, PathBuf),
    Cancelled,
    UnsafeName(String),
    TooLarge,
}

impl Failure {
    /// What the user is told about this failure. A phone error is mapped as it
    /// is, without the session handling `Service::to_error` adds, which is
    /// right for a failure about one file (the session is fine after one).
    pub(super) fn into_error(self) -> Error {
        match self {
            Failure::Device(err) => crate::service::mapper::to_error(err, false),
            Failure::Mac(err, path) => local::mapper::to_error(&err, &path.to_string_lossy()),
            Failure::Cancelled => Error::Cancelled,
            Failure::UnsafeName(name) => Error::UnsafeName(name),
            Failure::TooLarge => Error::TransferTooLarge,
        }
    }

    /// Whether this ends the whole batch. A failure about one file (the phone
    /// answered, but about that one object; the Mac refused that one file)
    /// lets the rest carry on. One about the connection, the Mac's disk or the
    /// user's cancel would only repeat for every file after it.
    pub(super) fn stops_the_batch(&self) -> bool {
        match self {
            Failure::Cancelled | Failure::UnsafeName(_) | Failure::TooLarge => true,
            Failure::Device(err) => !is_about_one_object(err),
            Failure::Mac(err, _) => matches!(local::mapper::to_error(err, ""), Error::MacDiskFull),
        }
    }
}

impl Service {
    pub fn new(device: Arc<device::Service>, explorer: Arc<explorer::Service>) -> Self {
        Self {
            limits: explorer::Limits::tied_to(device.activity()),
            device,
            explorer,
            stall_limit: STALL_LIMIT,
            trash: None,
            imports: None,
            mirror_reading: tokio::sync::Mutex::new(None),
            preview_cache: None,
            space_scan: tokio::sync::Mutex::new(None),
            editing: None,
            room: Arc::new(super::room::mac_room),
        }
    }

    /// The same service, told the Mac has `room` wherever it looks (tests).
    #[cfg(test)]
    pub(super) fn with_room(mut self, room: super::room::Room) -> Self {
        self.room = Arc::new(move |_| Ok(room.clone()));
        self
    }

    /// The same service giving up on a quiet phone after `limit`.
    #[cfg(test)]
    pub(super) fn with_stall_limit(mut self, limit: Duration) -> Self {
        self.stall_limit = limit;
        self
    }

    /// Copies `request.names` from the phone into a Mac folder, folders with
    /// everything in them. Nothing at the destination is overwritten.
    ///
    /// - A file already there with the same size and date is left alone, so
    ///   retrying a batch copies only what is missing.
    /// - A different file with the same name is kept, and the new one gets a
    ///   number, as Finder's "keep both" does.
    /// - A folder that exists is merged into.
    /// - Each file is written to a hidden part file and renamed into place
    ///   only when complete, so a half file never has a real name. When the
    ///   phone stops or the user cancels, the part file is kept (stamped with
    ///   the phone's date), and the next attempt carries on from it.
    ///
    /// Files keep the phone's modified date. `progress` is called after every
    /// chunk.
    pub async fn download(
        &self,
        request: &DownloadRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        self.download_as(Paste::Copy, request, cancel, progress)
            .await
    }

    /// Like `download`; with `Paste::Move`, each file's original is removed
    /// from the phone once its copy is known to be whole (see
    /// `remove_originals`), and the summary says what was removed and what was
    /// kept.
    pub async fn download_as(
        &self,
        mode: Paste,
        request: &DownloadRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        self.download_with(mode, request, Conflict::KeepBoth, cancel, progress)
            .await
    }

    /// Which of a download's selected names already hold something different
    /// in the destination: a folder of that name (merge or leave alone?), or
    /// a different file; the very file already there (same size and date)
    /// is no question, since it is not copied again. Asked before anything
    /// is copied, as uploads ask the phone.
    pub async fn taken_on_mac(&self, request: &DownloadRequest) -> Result<Vec<String>, Error> {
        let folder = self
            .explorer
            .open_folder(request.storage_id, &request.source)
            .await?;
        let destination = Path::new(&request.destination);
        let mut taken = Vec::new();
        for name in &request.names {
            let (Ok(safe), Some(on_phone)) = (safe_name(name), folder.child_named(name)) else {
                continue;
            };
            let Ok(there) = tokio::fs::symlink_metadata(destination.join(safe)).await else {
                continue;
            };
            let node = Node::from(on_phone);
            let when = node.modified.and_then(mapper::to_system_time);
            let identical =
                !node.is_folder && when.is_some_and(|when| is_same_file(&there, node.size, when));
            if !identical {
                taken.push(name.clone());
            }
        }
        Ok(taken)
    }

    /// `download_as`, doing about names taken in the destination what
    /// `conflict` says: Keep both numbers a different file (as before), Skip
    /// leaves a taken item alone, Replace (and Replace older, only for a
    /// newer phone file) stages the copy and swaps it in once it is whole,
    /// the old one going to the Trash; folders merge.
    pub async fn download_with(
        &self,
        mode: Paste,
        request: &DownloadRequest,
        conflict: Conflict,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let mut done = self
            .download_run_onward_with::<Nowhere>(
                mode == Paste::Move,
                request,
                conflict,
                cancel,
                &progress,
                None,
            )
            .await?;
        if mode == Paste::Move {
            let mut fates = vec![None; done.plan.steps.len()];
            done.summary.removal = Some(
                self.remove_originals(
                    &done.storage,
                    request,
                    &done.plan,
                    &done.outcomes,
                    cancel,
                    &progress,
                    &mut fates,
                )
                .await,
            );
            let (report, _) = report(
                &done.plan,
                &done.outcomes,
                &done.summary.failures,
                Some(&fates),
            );
            done.summary.report = report;
        }
        Ok(done.summary)
    }

    /// `download_run`, handing each folder and file to `onward` as soon as it
    /// is on the Mac (a copy through the Mac sends it back to the phone, one
    /// file at a time) and deleting each file afterwards. A file `onward` could
    /// not take is a failure of that file, like one that could not be read.
    pub(super) async fn download_run_onward<O: Onward>(
        &self,
        moving: bool,
        request: &DownloadRequest,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
        onward: Option<&mut O>,
    ) -> Result<Downloaded, Error> {
        self.download_run_onward_with(
            moving,
            request,
            Conflict::KeepBoth,
            cancel,
            progress,
            onward,
        )
        .await
    }

    pub(super) async fn download_run_onward_with<O: Onward>(
        &self,
        moving: bool,
        request: &DownloadRequest,
        conflict: Conflict,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
        onward: Option<&mut O>,
    ) -> Result<Downloaded, Error> {
        let started = Instant::now();
        let destination = PathBuf::from(&request.destination);
        match tokio::fs::metadata(&destination).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(Error::PathNotFound(request.destination.clone())),
            Err(err) => return Err(local::mapper::to_error(&err, &request.destination)),
        }

        // Only the selected files are asked about, not every file in the
        // folder: a camera folder can hold thousands.
        let folder = self
            .explorer
            .open_folder_confirmed(request.storage_id, &request.source, &request.names, true)
            .await?;
        let selected = request
            .names
            .iter()
            .map(|name| folder.exact_child(&request.source, name))
            .collect::<Result<Vec<_>, _>>()?;
        let partial_reads = folder.session.capabilities().supports_partial_download;

        let selected: Vec<Node> = selected.iter().map(Node::from).collect();
        let plan = match plan_with(
            &Phone {
                storage: &folder.storage,
                limits: &self.limits,
            },
            &selected,
            &destination,
            partial_reads,
            conflict,
        )
        .await
        {
            Ok(plan) => plan,
            Err(failure) => return Err(self.to_error(failure).await),
        };
        // Nothing on the Mac is replaced without a Trash for the old one:
        // refused before anything is copied.
        let replacing = plan.replaces.iter().any(Option::is_some);
        if replacing && self.trash.is_none() {
            return Err(Error::Unsupported);
        }
        for staged in plan
            .steps
            .iter()
            .zip(&plan.replaces)
            .filter_map(|(step, old)| match (step, old) {
                (Step::File { target, .. }, Some(_)) => target.parent(),
                _ => None,
            })
        {
            if let Err(err) = tokio::fs::create_dir_all(staged).await {
                return Err(local::mapper::to_error(&err, &staged.to_string_lossy()));
            }
        }

        let Executed {
            state,
            mut outcomes,
            already_there,
            resumed,
            not_copied,
            mut failures,
        } = self
            .run_steps(
                &plan,
                Run {
                    storage: &folder.storage,
                    partial_reads,
                    destination: &destination,
                    moving,
                    started,
                    cancel,
                    onward,
                },
                progress,
            )
            .await?;
        let replaced = if replacing {
            self.swap_in_replacements(&plan, &mut outcomes, &destination, &mut failures)
                .await
        } else {
            0
        };
        let copied = state.files_done - already_there - not_copied;
        if let Some(error) = whole_batch_failure(copied, already_there, &failures) {
            return Err(error);
        }
        let summary = TransferSummary {
            compared: None,
            direction: Direction::Download,
            files: copied,
            bytes: state.bytes_done,
            renamed: plan.renamed,
            skipped: plan.skipped.len(),
            replaced,
            merged: 0,
            already_there,
            resumed,
            failures,
            destination: request.destination.clone(),
            first_item: plan.first_item.clone(),
            removal: None,
            through_mac: false,
            mirrored: None,
            arrival: Some(arrival(&plan, &outcomes)),
            report: Vec::new(),
            report_left_out: 0,
            elapsed: std::time::Duration::ZERO,
        };
        let (report, report_left_out) = report(&plan, &outcomes, &summary.failures, None);
        let summary = TransferSummary {
            report,
            report_left_out,
            ..summary
        };
        Ok(Downloaded {
            summary,
            plan,
            outcomes,
            storage: folder.storage,
        })
    }

    /// Puts each whole staged copy in place of the Mac file it replaces, the
    /// old one set aside and then sent to the Trash with the others in one
    /// call; a copy not known to be whole leaves the old one, and a swap
    /// that fails is that file's failure and its step counts as not copied
    /// (so a move keeps its original). Says how many were replaced.
    async fn swap_in_replacements(
        &self,
        plan: &Plan,
        outcomes: &mut [Outcome],
        destination: &Path,
        failures: &mut Vec<FileFailure>,
    ) -> usize {
        let outgoing = destination.join(format!(
            ".open-mtp-client-replaced-{}",
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_millis())
        ));
        let mut replaced = 0;
        for ((step, old), outcome) in plan
            .steps
            .iter()
            .zip(&plan.replaces)
            .zip(outcomes.iter_mut())
        {
            let (
                Step::File {
                    target: staged,
                    label,
                    ..
                },
                Some(old),
            ) = (step, old)
            else {
                continue;
            };
            if *outcome != Outcome::Verified {
                continue;
            }
            let aside = outgoing.join(old.strip_prefix(destination).unwrap_or(old));
            match super::mirror::swap_in(staged, old, &aside, old).await {
                Ok(()) => replaced += 1,
                Err(err) => {
                    *outcome = Outcome::NotCopied;
                    failures.push(FileFailure {
                        label: label.clone(),
                        error: local::mapper::to_error(&err, label),
                    });
                }
            }
        }
        if replaced > 0
            && let Some(trash) = self.trash.clone()
        {
            let folder = outgoing.clone();
            let trashed = tokio::task::spawn_blocking(move || trash(&folder))
                .await
                .unwrap_or_else(|_| Err(std::io::Error::other("the Trash did not answer")));
            if let Err(err) = trashed {
                failures.push(FileFailure {
                    label: file_name(&outgoing),
                    error: local::mapper::to_error(&err, &outgoing.to_string_lossy()),
                });
            }
        }
        // What is staged and not swapped in (a part to carry on from) stays.
        super::mirror::remove_empty_folders(&destination.join(DOWNLOAD_STAGING)).await;
        replaced
    }

    /// Carries out a plan's steps in order: makes each folder and copies each
    /// file (or finds it already there, or carries on from its part file),
    /// noting how each went. A failure about one file is noted and the rest go
    /// on; one that stops the batch (`Failure::stops_the_batch`) or a cancel
    /// ends it. Shared by downloads, moves to the Mac, copies through the Mac
    /// and imports.
    pub(super) async fn run_steps<O: Onward>(
        &self,
        plan: &Plan,
        run: Run<'_, O>,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) -> Result<Executed, Error> {
        let Run {
            storage,
            partial_reads,
            destination,
            moving,
            started,
            cancel,
            mut onward,
        } = run;
        // Room first, before a byte is written: what is still to copy must
        // fit on the Mac; a copy through the Mac holds one file there at a
        // time, and must fit where it goes on.
        let mac_room = (self.room)(destination).ok();
        // A disk that can't be written refuses before anything; a file too
        // big for its format (4 GB or more on FAT32) is that file's failure,
        // and the rest go.
        super::room::ensure_writable(mac_room.as_ref())?;
        let too_big = |step: &Step| match step {
            Step::File {
                size,
                how: How::Fresh | How::Resume { .. },
                ..
            } => super::room::too_big(*size, mac_room.as_ref()),
            _ => false,
        };
        let to_write: u64 = plan
            .steps
            .iter()
            .filter(|step| !too_big(step))
            .map(Step::bytes_to_copy)
            .sum();
        match &onward {
            Some(onward) => {
                let biggest = plan
                    .steps
                    .iter()
                    .map(Step::bytes_to_copy)
                    .max()
                    .unwrap_or(0);
                super::room::ensure(biggest, mac_room.as_ref())?;
                super::room::ensure(to_write, onward.room().as_ref())?;
            }
            None => super::room::ensure(to_write, mac_room.as_ref())?,
        }
        // Only a move that keeps its copy on the Mac, and so deletes the
        // original, needs each file on the disk before it goes on (`Copy`).
        let durable = moving && onward.is_none();
        let mut state = TransferProgress {
            moving,
            files_total: plan
                .steps
                .iter()
                .filter(|s| matches!(s, Step::File { .. }))
                .count(),
            // Only what is still to move: what is already there, or already
            // copied by an earlier attempt, would make the speed look better
            // than it is.
            bytes_total: plan.steps.iter().map(Step::bytes_to_copy).sum(),
            ..TransferProgress::default()
        };

        let (mut already_there, mut resumed) = (0, 0);
        // Files that did not make it, and folders that could not be created
        // (everything inside one is skipped without a failure of its own).
        let (mut not_copied, mut failures) = (0, Vec::new());
        let mut failed_folders: Vec<&Path> = Vec::new();
        let mut outcomes = vec![Outcome::NotCopied; plan.steps.len()];
        for (index, step) in plan.steps.iter().enumerate() {
            // A cancel also stops the batch between files, not only within one.
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Step::File { target, .. } = step
                && failed_folders
                    .iter()
                    .any(|folder| target.starts_with(folder))
            {
                state.bytes_total = state.bytes_total.saturating_sub(step.bytes_to_copy());
                state.files_done += 1;
                not_copied += 1;
                continue;
            }
            if let Step::File { label, .. } = step
                && too_big(step)
            {
                state.bytes_total = state.bytes_total.saturating_sub(step.bytes_to_copy());
                state.files_done += 1;
                not_copied += 1;
                let disk = mac_room.as_ref().map(|room| room.disk.clone());
                failures.push(FileFailure {
                    label: label.clone(),
                    error: Error::TooBigForVolume(disk.unwrap_or_default()),
                });
                continue;
            }
            let result = match step {
                Step::Folder(path) => match tokio::fs::create_dir_all(path).await {
                    Err(err) => Err(Failure::Mac(err, path.clone())),
                    Ok(()) => match onward.as_deref_mut() {
                        Some(onward) => onward
                            .folder(path.strip_prefix(destination).unwrap_or(path))
                            .await
                            .map(|()| Outcome::Verified),
                        None => Ok(Outcome::Verified),
                    },
                },
                // The same size and date is as good as a copy just made, so a
                // retried move can go on to remove what an earlier one copied.
                Step::File {
                    how: How::AlreadyThere,
                    size,
                    target,
                    ..
                } => {
                    // A copy that did not wait for the disk may have written
                    // it, and a move is about to delete the original on the
                    // strength of it: it waits now.
                    let settled = if durable {
                        force_to_disk(target).await
                    } else {
                        Ok(())
                    };
                    match settled {
                        Ok(()) => {
                            already_there += 1;
                            Ok(Outcome::of_existing(*size))
                        }
                        Err(err) => Err(Failure::Mac(err, target.clone())),
                    }
                }
                Step::File {
                    target,
                    size,
                    modified,
                    ..
                } if onward.as_deref_mut().is_some_and(|onward| {
                    onward.has(
                        target.strip_prefix(destination).unwrap_or(target),
                        *size,
                        modified.and_then(mapper::to_system_time),
                    )
                }) =>
                {
                    // The phone has it already (an earlier try of this move
                    // sent it): nothing to read, nothing to send.
                    already_there += 1;
                    Ok(Outcome::of_existing(*size))
                }
                Step::File {
                    handle,
                    target,
                    label,
                    size,
                    modified,
                    how,
                } => {
                    state.current_file = label.clone();
                    state.finishing = false;
                    state.file_bytes_total = *size;
                    state.file_bytes_done = match how {
                        How::Resume { offset, .. } => *offset,
                        _ => 0,
                    };
                    state.elapsed = started.elapsed();
                    progress(state.clone());

                    let copy = Copy {
                        storage,
                        handle: *handle,
                        partial_reads,
                        cancel,
                        progress: &progress,
                        started,
                        stall_limit: self.stall_limit,
                        heard: self.device.activity(),
                        moving,
                        relayed: onward.is_some(),
                    };
                    match copy.to(target, how, *modified, &mut state).await {
                        Ok(copied) => {
                            resumed += usize::from(copied.resumed);
                            let outcome = if copied.verified {
                                Outcome::Verified
                            } else {
                                Outcome::Unchecked
                            };
                            match onward.as_deref_mut() {
                                None => Ok(outcome),
                                Some(onward) => {
                                    let sent = onward
                                        .file(
                                            target.strip_prefix(destination).unwrap_or(target),
                                            target,
                                        )
                                        .await;
                                    // Whatever happened, the Mac does not keep
                                    // the file: that is the point of going on
                                    // one at a time.
                                    let _ = tokio::fs::remove_file(target).await;
                                    sent.map(|()| outcome)
                                }
                            }
                        }
                        Err(failure) => Err(failure),
                    }
                }
            };
            if let Ok(outcome) = &result {
                outcomes[index] = *outcome;
            }
            if let Err(failure) = result {
                if failure.stops_the_batch() {
                    return Err(self.to_error(failure).await);
                }
                // About this one only: note it, and carry on with the rest.
                let label = match step {
                    Step::File { label, .. } => label.clone(),
                    Step::Folder(path) => path
                        .strip_prefix(destination)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned(),
                };
                failures.push(FileFailure {
                    label,
                    error: self.to_error(failure).await,
                });
                match step {
                    Step::File { .. } => {
                        // What this file didn't send is no longer owed.
                        let unsent = state.file_bytes_total.saturating_sub(state.file_bytes_done);
                        state.bytes_total = state.bytes_total.saturating_sub(unsent);
                        not_copied += 1;
                    }
                    Step::Folder(path) => failed_folders.push(path),
                }
            }
            if matches!(step, Step::File { .. }) {
                state.files_done += 1;
            }
        }

        state.elapsed = started.elapsed();
        progress(state.clone());
        Ok(Executed {
            state,
            outcomes,
            already_there,
            resumed,
            not_copied,
            failures,
        })
    }

    pub(super) async fn to_error(&self, failure: Failure) -> Error {
        match failure {
            Failure::Device(err) => self.device.handle_error(err).await,
            other => other.into_error(),
        }
    }

    /// After a move's copies are checked, removes from the phone exactly the
    /// files whose copy was verified, and nothing else.
    ///
    /// Android deletes a folder together with everything in it, so a folder is
    /// never removed for its contents. Files go one at a time, each looked up
    /// again first (it must still be the same file, unchanged since it was
    /// copied, and not a handle that was re-keyed to something else), and a
    /// folder goes only when everything planned under it is gone and the phone
    /// says it is empty: a photo taken meanwhile stays. Whatever stays is
    /// reported with the reason. A phone that stops answering, or a cancel,
    /// ends the removal with the rest left alone.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn remove_originals(
        &self,
        storage: &Storage,
        request: &DownloadRequest,
        plan: &Plan,
        outcomes: &[Outcome],
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
        fates: &mut [Option<Original>],
    ) -> Removal {
        let mut removal = Removal::default();
        // Folders (relative to the selection's) that must stay, because
        // something inside them did.
        let mut blocked: HashSet<String> = HashSet::new();
        let mut top_gone = vec![false; plan.items.len()];
        // What is gone, by the phone folder it was in, for the pane's memory.
        let mut gone: BTreeMap<String, Vec<ObjectHandle>> = BTreeMap::new();
        let mut gone_folders: Vec<String> = Vec::new();

        let owed = plan
            .origins
            .iter()
            .zip(outcomes)
            .filter(|(origin, outcome)| !origin.is_folder && **outcome == Outcome::Verified)
            .count();
        let started = Instant::now();
        let mut state = TransferProgress {
            direction: Direction::Download,
            moving: true,
            removing: true,
            files_total: owed,
            ..TransferProgress::default()
        };
        progress(state.clone());

        // Planned folder first, then what is in it: backwards, what is in a
        // folder is dealt with before the folder.
        for (index, origin) in plan.origins.iter().enumerate().rev() {
            let path = join_label(&origin.parent, &origin.name);
            let outcome = outcomes[index];
            if !should_remove(origin, outcome, plan.unwalked, &blocked) {
                if !origin.is_folder && outcome == Outcome::Unchecked {
                    note_fate(fates, index, Original::Kept(Why::Unchecked));
                    removal.kept.push(Kept {
                        label: path,
                        why: Why::Unchecked,
                    });
                }
                block(&mut blocked, &origin.parent);
                continue;
            }
            if removal.stopped.is_none() && cancel.is_cancelled() {
                removal.stopped = Some(Error::Cancelled);
            }
            if removal.stopped.is_some() {
                block(&mut blocked, &origin.parent);
                continue;
            }

            match self.remove_one(storage, origin).await {
                Removed::Done => {
                    note_fate(fates, index, Original::Removed);
                    gone.entry(absolute(&request.source, &origin.parent))
                        .or_default()
                        .push(origin.handle);
                    if origin.is_folder {
                        gone_folders.push(absolute(&request.source, &path));
                    } else {
                        removal.removed += 1;
                        state.files_done += 1;
                        state.elapsed = started.elapsed();
                        progress(state.clone());
                    }
                    if origin.is_top {
                        top_gone[origin.top] = true;
                    }
                }
                Removed::Kept(why) => {
                    note_fate(fates, index, Original::Kept(why.clone()));
                    removal.kept.push(Kept { label: path, why });
                    block(&mut blocked, &origin.parent);
                }
                Removed::Stopped(failure) => {
                    removal.stopped = Some(self.to_error(failure).await);
                    block(&mut blocked, &origin.parent);
                }
            }
        }

        self.explorer
            .note_removed(request.storage_id, &gone, &gone_folders)
            .await;
        // Found backwards; shown in the order the files were copied.
        removal.kept.reverse();
        removal.left = plan
            .items
            .iter()
            .zip(&top_gone)
            .filter(|(_, gone)| !**gone)
            .map(|(name, _)| name.clone())
            .collect();
        removal
    }

    /// Removes one original, if it is still the one that was copied.
    pub(super) async fn remove_one(&self, storage: &Storage, origin: &Origin) -> Removed {
        let limit = self.stall_limit;
        let activity = self.device.activity();
        let info =
            match phone_answers(&activity, limit, storage.get_object_info(origin.handle)).await {
                Err(failure) => return Removed::Stopped(failure),
                Ok(Err(err)) => return refused(err),
                Ok(Ok(info)) => info,
            };
        let unchanged = info.filename == origin.name
            && info.is_folder() == origin.is_folder
            && (origin.is_folder || (info.size == origin.size && info.modified == origin.modified));
        if !unchanged {
            return Removed::Kept(Why::Changed);
        }
        if origin.is_folder {
            // The phone's own count of what is in it, not the items that could
            // be described: a file the phone lists but won't describe was never
            // planned, so never copied, and must still count as "something in it".
            match phone_answers(
                &activity,
                limit,
                storage.list_objects_stream(Some(origin.handle)),
            )
            .await
            {
                Err(failure) => return Removed::Stopped(failure),
                Ok(Err(err)) => return refused(err),
                Ok(Ok(listing)) if listing.total() > 0 => {
                    return Removed::Kept(Why::NotEmpty);
                }
                Ok(Ok(_)) => {}
            }
        }
        match phone_answers(
            &activity,
            limit,
            explorer::delete_object(storage, &self.limits, origin.handle, false),
        )
        .await
        {
            Err(failure) => Removed::Stopped(failure),
            Ok(Err(err)) => refused(err),
            Ok(Ok(())) => Removed::Done,
        }
    }
}

/// Whether a move goes on to remove this original: a file whose copy was
/// checked, or a folder that was copied and has nothing staying inside it. A
/// folder that was only partly walked (a cycle in the phone's tree) never goes.
fn should_remove(
    origin: &Origin,
    outcome: Outcome,
    unwalked: bool,
    blocked: &HashSet<String>,
) -> bool {
    match (origin.is_folder, outcome) {
        (false, Outcome::Verified) => true,
        (true, Outcome::Verified) => {
            !unwalked && !blocked.contains(&join_label(&origin.parent, &origin.name))
        }
        _ => false,
    }
}

/// What came of trying to remove one original.
pub(super) enum Removed {
    Done,
    /// It stays, for this reason.
    Kept(Why),
    /// The phone stopped answering: nothing more is tried.
    Stopped(Failure),
}

/// The phone answered no about this one object, which then stays; any other
/// failure is about the connection, and ends the removal.
fn refused(err: mtp_rs::Error) -> Removed {
    match err {
        // The object is not what was copied any more: it vanished, or the
        // phone gave it a new handle.
        mtp_rs::Error::NotFound | mtp_rs::Error::StaleHandle => Removed::Kept(Why::Changed),
        err if is_about_one_object(&err) => {
            Removed::Kept(Why::Refused(crate::service::mapper::to_error(err, false)))
        }
        err => Removed::Stopped(Failure::Device(err)),
    }
}

/// Marks `folder` and every folder above it (all relative to the selection's
/// folder) as one that must stay.
fn block(blocked: &mut HashSet<String>, mut folder: &str) {
    while !folder.is_empty() {
        blocked.insert(folder.to_owned());
        folder = folder.rsplit_once('/').map_or("", |(above, _)| above);
    }
}

/// The phone path of `relative` inside the selection's folder `source`.
fn absolute(source: &str, relative: &str) -> String {
    if relative.is_empty() {
        source.to_owned()
    } else {
        format!("{}/{relative}", source.trim_end_matches('/'))
    }
}

/// Deepest folder nesting a transfer follows. Phones nest a handful of
/// levels; far past this is a broken or hostile device.
pub(super) const MAX_DEPTH: usize = 64;
/// Most files and folders one transfer plans.
pub(super) const MAX_ITEMS: usize = 1_000_000;

/// A device object as planning needs it.
#[derive(Debug, Clone)]
struct Node {
    handle: ObjectHandle,
    name: String,
    is_folder: bool,
    size: u64,
    modified: Option<DateTime>,
}

impl From<&ObjectInfo> for Node {
    fn from(object: &ObjectInfo) -> Self {
        Self {
            handle: object.handle,
            name: object.filename.clone(),
            is_folder: object.is_folder(),
            size: object.size,
            modified: object.modified,
        }
    }
}

/// What planning needs from a device storage; a seam so hostile trees
/// (cycles, `../` names, absurd depth) can be tested without a device.
trait Children: Sync {
    fn children(
        &self,
        folder: ObjectHandle,
    ) -> impl Future<Output = Result<Vec<Node>, mtp_rs::Error>> + Send;
}

/// A device storage, with the limits its requests are made under.
struct Phone<'a> {
    storage: &'a Storage,
    limits: &'a explorer::Limits,
}

impl Children for Phone<'_> {
    async fn children(&self, folder: ObjectHandle) -> Result<Vec<Node>, mtp_rs::Error> {
        // Each item gets its own limit: a camera folder is thousands of requests.
        let children = explorer::read_children(self.storage, Some(folder), self.limits).await?;
        Ok(children.iter().map(Node::from).collect())
    }
}

/// Lists everything under the selection so totals are known before the
/// first byte, and decides what happens to each file (copy, resume or skip)
/// and where it goes. The device is not trusted: every name it reports is
/// checked, every target must stay inside the destination, folders seen twice
/// are skipped and the walk is bounded, so a bad device stops the copy before
/// anything is written.
#[cfg(test)]
async fn plan(
    device: &impl Children,
    selected: &[Node],
    destination: &Path,
    partial_reads: bool,
) -> Result<Plan, Failure> {
    plan_with(
        device,
        selected,
        destination,
        partial_reads,
        Conflict::KeepBoth,
    )
    .await
}

async fn plan_with(
    device: &impl Children,
    selected: &[Node],
    destination: &Path,
    partial_reads: bool,
    conflict: Conflict,
) -> Result<Plan, Failure> {
    let mut walker = Walker {
        conflict,
        replaces: Vec::new(),
        replacing: HashSet::new(),
        skipped: Vec::new(),
        device,
        destination,
        steps: Vec::new(),
        origins: Vec::new(),
        phone_parent: String::new(),
        top: 0,
        unwalked: false,
        visited: HashSet::new(),
        max_depth: MAX_DEPTH,
        max_items: MAX_ITEMS,
        partial_reads,
    };
    let mut renamed = 0;
    let mut first_item = None;
    for (top, node) in selected.iter().enumerate() {
        walker.top = top;
        let (name, was_renamed) = walker.walk(node, destination, "", 0).await?;
        renamed += usize::from(was_renamed);
        first_item.get_or_insert(name);
    }
    Ok(Plan {
        steps: walker.steps,
        origins: walker.origins,
        items: selected.iter().map(|node| node.name.clone()).collect(),
        unwalked: walker.unwalked,
        renamed,
        first_item,
        replaces: walker.replaces,
        skipped: walker.skipped,
    })
}

/// What the answer about taken names makes of one item whose name is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Taken {
    /// As before the question existed: a folder merges, a different file is
    /// numbered (or found already there).
    AsBefore,
    /// Left alone.
    Skip,
    /// Staged, and swapped in for the Mac file once whole.
    Replace,
}

/// The phone's file is newer than the Mac's, by more than the two seconds
/// either side may round to; an unknown date on either side is not newer.
fn phone_is_newer(modified: Option<DateTime>, there: &std::fs::Metadata) -> bool {
    let phone = modified.and_then(mapper::to_system_time);
    matches!(
        (phone, there.modified().ok()),
        (Some(phone), Some(mac)) if phone > mac && !mapper::same_moment(phone, mac)
    )
}

struct Walker<'a, D> {
    /// What to do about a name taken in the destination.
    conflict: Conflict,
    /// One per step: the Mac file a staged copy replaces.
    replaces: Vec<Option<PathBuf>>,
    /// Mac files already planned to be replaced, and staged copies, in lower
    /// case (APFS ignores case): two phone names differing only in case
    /// must not both replace one Mac file, or the second swap would set the
    /// first's copy aside over the original.
    replacing: HashSet<String>,
    skipped: Vec<String>,
    device: &'a D,
    destination: &'a Path,
    steps: Vec<Step>,
    /// One per step: where its original is on the phone.
    origins: Vec<Origin>,
    /// The phone folder being walked, relative to the selection's folder.
    phone_parent: String,
    /// The selected item being walked.
    top: usize,
    /// A folder was met twice and skipped.
    unwalked: bool,
    visited: HashSet<u64>,
    max_depth: usize,
    max_items: usize,
    partial_reads: bool,
}

impl<D: Children> Walker<'_, D> {
    /// What the answer makes of `node` going to `base`: Keep both (and a
    /// retry's merge) as before; a free name, or the very file already
    /// there, as before; a taken item at the top skipped by Skip; a different
    /// file replaced by Replace, and by Replace older only for a newer phone
    /// file. A folder merges into a folder; under Skip at the top, or under
    /// Replace with something other than a folder (or a file) in the way, the
    /// item is left alone: nothing of one kind replaces the other.
    async fn taken(&self, node: &Node, base: &Path, depth: usize) -> Taken {
        if matches!(self.conflict, Conflict::KeepBoth | Conflict::Merge) {
            return Taken::AsBefore;
        }
        let Ok(there) = tokio::fs::symlink_metadata(base).await else {
            return Taken::AsBefore;
        };
        let when = node.modified.and_then(mapper::to_system_time);
        if !node.is_folder && when.is_some_and(|when| is_same_file(&there, node.size, when)) {
            return Taken::AsBefore;
        }
        match self.conflict {
            Conflict::Skip if depth == 0 => Taken::Skip,
            Conflict::Skip => Taken::AsBefore,
            _ if node.is_folder => {
                if there.is_dir() {
                    Taken::AsBefore
                } else {
                    Taken::Skip
                }
            }
            _ if !there.is_file() => Taken::Skip,
            Conflict::ReplaceOlder if !phone_is_newer(node.modified, &there) => Taken::Skip,
            _ => Taken::Replace,
        }
    }

    /// Where `node`'s original is on the phone, for a move to find it again.
    fn origin(&self, node: &Node, depth: usize) -> Origin {
        Origin {
            handle: node.handle,
            parent: self.phone_parent.clone(),
            name: node.name.clone(),
            size: node.size,
            modified: node.modified,
            is_folder: node.is_folder,
            top: self.top,
            is_top: depth == 0,
        }
    }

    /// Plans `node` inside the folder `parent`. Returns the name it gets
    /// there, and whether that is a new name because its own was taken by
    /// something different.
    async fn walk(
        &mut self,
        node: &Node,
        parent: &Path,
        parent_label: &str,
        depth: usize,
    ) -> Result<(String, bool), Failure> {
        let name = safe_name(&node.name)?;
        let base = parent.join(name);
        if !base.starts_with(self.destination) {
            return Err(Failure::UnsafeName(join_label(parent_label, name)));
        }
        if depth > self.max_depth || self.steps.len() >= self.max_items {
            return Err(Failure::TooLarge);
        }

        match self.taken(node, &base, depth).await {
            Taken::AsBefore => {}
            Taken::Skip => {
                self.skipped.push(join_label(parent_label, name));
                return Ok((node.name.clone(), false));
            }
            Taken::Replace => {
                // Another item of this run replaces that file already.
                if !self.replacing.insert(base.to_string_lossy().to_lowercase()) {
                    self.skipped.push(join_label(parent_label, name));
                    return Ok((node.name.clone(), false));
                }
                let staged = self
                    .destination
                    .join(DOWNLOAD_STAGING)
                    .join(base.strip_prefix(self.destination).unwrap_or(&base));
                let (target, how) = resolve_file(
                    &staged,
                    node.size,
                    node.modified,
                    self.partial_reads,
                    &self.replacing,
                )
                .await;
                self.replacing
                    .insert(target.to_string_lossy().to_lowercase());
                self.origins.push(self.origin(node, depth));
                self.replaces.push(Some(base));
                self.steps.push(Step::File {
                    handle: node.handle,
                    label: join_label(parent_label, &node.name),
                    target,
                    size: node.size,
                    modified: node.modified,
                    how,
                });
                return Ok((node.name.clone(), false));
            }
        }
        if !node.is_folder {
            let (target, how) = resolve_file(
                &base,
                node.size,
                node.modified,
                self.partial_reads,
                &HashSet::new(),
            )
            .await;
            let final_name = file_name(&target);
            let renamed = final_name != node.name && how != How::AlreadyThere;
            self.origins.push(self.origin(node, depth));
            self.replaces.push(None);
            self.steps.push(Step::File {
                handle: node.handle,
                label: join_label(parent_label, &final_name),
                target,
                size: node.size,
                modified: node.modified,
                how,
            });
            return Ok((final_name, renamed));
        }
        // A folder listed twice would be a cycle in the device's tree.
        if !self.visited.insert(node.handle.0) {
            self.unwalked = true;
            return Ok((node.name.clone(), false));
        }

        let target = folder_target(&base).await;
        let final_name = file_name(&target);
        let label = join_label(parent_label, &final_name);
        self.origins.push(self.origin(node, depth));
        self.replaces.push(None);
        self.steps.push(Step::Folder(target.clone()));
        let children = self
            .device
            .children(node.handle)
            .await
            .map_err(Failure::Device)?;
        let inside = join_label(&self.phone_parent, &node.name);
        let outer = std::mem::replace(&mut self.phone_parent, inside);
        for child in &children {
            Box::pin(self.walk(child, &target, &label, depth + 1)).await?;
        }
        self.phone_parent = outer;
        let renamed = final_name != node.name;
        Ok((final_name, renamed))
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn join_label(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// Where a folder goes: an existing real folder is merged into, a free name
/// is created, and anything else with that name (a file, or a link to a
/// folder, which could lead outside the destination) is left alone and the
/// folder gets the next free number.
async fn folder_target(base: &Path) -> PathBuf {
    let name = file_name(base);
    let parent = base.parent().unwrap_or(Path::new("/"));
    let mut n = 0;
    loop {
        let candidate = parent.join(mapper::numbered(&name, n, true));
        match tokio::fs::symlink_metadata(&candidate).await {
            Err(err) if err.kind() == ErrorKind::NotFound => return candidate,
            Ok(metadata) if metadata.is_dir() => return candidate,
            _ => n += 1,
        }
    }
}

/// Decides what to do about one device file and where it goes: `base`, or
/// `name (1)`, `name (2)`… if something different is in the way. The same
/// file (size and date) under any of those names means it was copied before.
/// At the first free name, a part file left by an earlier attempt is carried
/// on from when the phone can read from an offset. A name in `claimed`
/// (lowercased: APFS ignores case) is taken by another file of the same plan.
pub(super) async fn resolve_file(
    base: &Path,
    size: u64,
    modified: Option<DateTime>,
    partial_reads: bool,
    claimed: &HashSet<String>,
) -> (PathBuf, How) {
    let when = modified.and_then(mapper::to_system_time);
    let name = file_name(base);
    let parent = base.parent().unwrap_or(Path::new("/"));
    let mut n = 0;
    loop {
        let target = parent.join(mapper::numbered(&name, n, false));
        if claimed.contains(&target.to_string_lossy().to_lowercase()) {
            n += 1;
            continue;
        }
        match tokio::fs::symlink_metadata(&target).await {
            // Not there, or it can't be (a file where a folder on the way
            // should be): the step that makes the folder will say why.
            Err(_) => {
                let how = match when.filter(|_| partial_reads) {
                    Some(when) => match resumable_part(&target, size, when).await {
                        Some((part, offset)) => How::Resume { part, offset },
                        None => How::Fresh,
                    },
                    None => How::Fresh,
                };
                return (target, how);
            }
            Ok(found) if when.is_some_and(|when| is_same_file(&found, size, when)) => {
                return (target, How::AlreadyThere);
            }
            _ => n += 1,
        }
    }
}

/// A real file (not a link) of exactly `size` bytes dated `when`.
pub(super) fn is_same_file(found: &std::fs::Metadata, size: u64, when: SystemTime) -> bool {
    found.is_file()
        && found.len() == size
        && found
            .modified()
            .is_ok_and(|modified| mapper::same_moment(modified, when))
}

/// How many part-file names are looked at for one target.
const PART_CANDIDATES: usize = 8;

/// A part file an earlier attempt left for `target`: a real file, shorter than
/// the whole, that was stamped with the phone's date for this very file when
/// the attempt stopped. A file that was changed on the phone since has a
/// different date, and then its part is not continued.
pub(super) async fn resumable_part(
    target: &Path,
    size: u64,
    when: SystemTime,
) -> Option<(PathBuf, u64)> {
    let name = file_name(target);
    for n in 0..PART_CANDIDATES {
        let part = target.with_file_name(mapper::part_name(&name, n));
        // Part names are handed out in order, so the first missing one is the
        // end of the list; a usual file has none, and costs one look.
        let Ok(found) = tokio::fs::symlink_metadata(&part).await else {
            return None;
        };
        let stamped = found
            .modified()
            .is_ok_and(|modified| mapper::same_moment(modified, when));
        if found.is_file() && stamped && found.len() > 0 && found.len() < size {
            return Some((part, found.len()));
        }
    }
    None
}

pub(super) fn safe_name(name: &str) -> Result<&str, Failure> {
    if names::is_safe_component(name) {
        Ok(name)
    } else {
        Err(Failure::UnsafeName(name.to_owned()))
    }
}

/// Creates the hidden file a download is written to before it is complete.
/// The create is exclusive: an existing file or symlink with that name is
/// never opened (a planted link would otherwise get its target truncated),
/// and a taken name moves on to the next one.
async fn create_part(target: &Path) -> std::io::Result<(PathBuf, tokio::fs::File)> {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut n = 0;
    loop {
        let part = target.with_file_name(mapper::part_name(&name, n));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
        {
            Ok(file) => return Ok((part, file)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => n += 1,
            Err(err) => return Err(err),
        }
    }
}

/// Opens a part file from an earlier attempt to carry on writing to it, only
/// if it is still exactly what the plan saw: the same regular file (not a link
/// swapped in since), `offset` bytes long. Nothing is written before this has
/// checked.
async fn open_resumable(part: &Path, offset: u64) -> Option<tokio::fs::File> {
    let file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(part)
        .await
        .ok()?;
    let opened = file.metadata().await.ok()?;
    let named = tokio::fs::symlink_metadata(part).await.ok()?;
    let same = named.is_file()
        && opened.is_file()
        && named.dev() == opened.dev()
        && named.ino() == opened.ino()
        && opened.len() == offset;
    same.then_some(file)
}

/// Waits for the phone, but only so long. A phone that has wedged can go
/// silent without reporting any error, and the copy would wait on it for
/// ever; giving up reports the same `Timeout` a slow USB transfer would.
pub(super) async fn phone_answers<T>(
    activity: &device::Activity,
    limit: Duration,
    work: impl Future<Output = T>,
) -> Result<T, Failure> {
    // On the session's shared clock: a request queued behind a long one (a
    // folder copy, another transfer's chunks) is not silent while that one
    // makes progress.
    activity
        .within(limit, false, work)
        .await
        .ok_or(Failure::Device(mtp_rs::Error::Timeout))
}

/// The most a file can send, from the size the phone reported. MTP reports
/// `0xFFFFFFFF` for "4 GB or more"; that is no size at all, so it limits
/// nothing.
pub(super) fn size_limit(reported: u64) -> u64 {
    if reported == u64::from(u32::MAX) {
        u64::MAX
    } else {
        reported
    }
}

/// What a run of `plan` brought: its files, how many are at the destination
/// (`Verified` or `Unchecked`), and how many of those were checked to be the
/// size the phone reported (`Verified`).
pub(super) fn arrival(plan: &Plan, outcomes: &[Outcome]) -> crate::dto::Arrival {
    let mut arrival = crate::dto::Arrival {
        checked: Some(0),
        complete: !plan.unwalked,
        ..crate::dto::Arrival::default()
    };
    for (step, outcome) in plan.steps.iter().zip(outcomes) {
        let Step::File { size, .. } = step else {
            continue;
        };
        arrival.files += 1;
        if size_is_known(*size) {
            arrival.bytes += size;
        }
        match outcome {
            Outcome::Verified => {
                arrival.there += 1;
                arrival.checked = arrival.checked.map(|checked| checked + 1);
            }
            Outcome::Unchecked => arrival.there += 1,
            Outcome::NotCopied => {}
        }
    }
    arrival
}

/// Notes what became of the original of step `index`, if a report wants it.
pub(super) fn note_fate(fates: &mut [Option<Original>], index: usize, fate: Original) {
    if let Some(slot) = fates.get_mut(index) {
        *slot = Some(fate);
    }
}

/// What became of each file of `plan`, for the transfer's report: one entry
/// per file in the order copied (the first `REPORT_MOST`; the rest are
/// counted), then the items the answer about taken names left alone.
/// `fates`, for a move, says what became of each step's original (unnoted:
/// it stayed).
pub(super) fn report(
    plan: &Plan,
    outcomes: &[Outcome],
    failures: &[FileFailure],
    fates: Option<&[Option<Original>]>,
) -> (Vec<FileReport>, usize) {
    let failed: HashMap<&str, &Error> = failures
        .iter()
        .map(|failure| (failure.label.as_str(), &failure.error))
        .collect();
    let original = |index: usize| {
        fates.map(|fates| {
            fates
                .get(index)
                .cloned()
                .flatten()
                .unwrap_or(Original::Stayed)
        })
    };
    let mut report = Vec::new();
    let mut left_out = 0;
    for (index, (step, outcome)) in plan.steps.iter().zip(outcomes).enumerate() {
        let Step::File { label, how, .. } = step else {
            continue;
        };
        if report.len() >= REPORT_MOST {
            left_out += 1;
            continue;
        }
        let replacing = plan.replaces.get(index).is_some_and(Option::is_some);
        let checked = *outcome == Outcome::Verified;
        let outcome = match (outcome, how) {
            (Outcome::NotCopied, _) => failed
                .get(label.as_str())
                .map_or(FileOutcome::NotCopied, |error| {
                    FileOutcome::Failed((*error).clone())
                }),
            (_, How::AlreadyThere) => FileOutcome::AlreadyThere,
            // Only a whole copy is swapped in for the old file.
            _ if replacing && checked => FileOutcome::Replaced,
            _ if replacing => FileOutcome::NotCopied,
            (_, How::Resume { .. }) => FileOutcome::Resumed { checked },
            (_, How::Fresh) => FileOutcome::Copied { checked },
        };
        let from = plan
            .origins
            .get(index)
            .map(|origin| join_label(&origin.parent, &origin.name))
            .filter(|from| from != label);
        report.push(FileReport {
            path: label.clone(),
            from,
            outcome,
            original: original(index),
        });
    }
    for name in &plan.skipped {
        if report.len() >= REPORT_MOST {
            left_out += 1;
            continue;
        }
        report.push(FileReport {
            path: name.clone(),
            from: None,
            outcome: FileOutcome::Skipped,
            original: fates.map(|_| Original::Stayed),
        });
    }
    (report, left_out)
}

/// Whether the phone said how big a file is, so a copy can be checked against it.
pub(super) fn size_is_known(reported: u64) -> bool {
    size_limit(reported) != u64::MAX
}

/// The phone is untrusted: a file that sends more than the size it reported
/// is a device error, found before the extra bytes are written. Without this
/// a phone could stream for ever past its own size, and a cancel waved
/// through on the strength of that size would never come.
pub(super) fn check_within_size(received: u64, incoming: usize, limit: u64) -> Result<(), Failure> {
    if received.saturating_add(incoming as u64) > limit {
        return Err(Failure::Device(mtp_rs::Error::invalid_data(
            "the phone sent more of a file than the size it reported",
        )));
    }
    Ok(())
}

/// Whether a cancel stops the copy now. Only a file that has arrived in full
/// is let through to be placed; anything short of that stops, and the phone
/// cannot make a file look complete, because it can't send past its size.
fn cancel_stops(cancelled: bool, received: u64, limit: u64) -> bool {
    cancelled && received != limit
}

/// What becomes of a part file when its copy stops. When the phone stopped
/// answering or the user cancelled, what was copied is kept and stamped with
/// the phone's date, which is how the next attempt knows it belongs to that
/// file. A Mac-side failure (disk full, no permission) would only happen
/// again, a file with nothing in it or no date has nothing to continue, and a
/// phone that can't read from an offset can't continue it either.
async fn settle_part(part: &Path, failure: &Failure, when: Option<SystemTime>, resumable: bool) {
    let worth_keeping = resumable && matches!(failure, Failure::Cancelled | Failure::Device(_));
    let copied = tokio::fs::metadata(part)
        .await
        .map_or(0, |metadata| metadata.len());
    if worth_keeping
        && copied > 0
        && let Some(when) = when
        && std::fs::File::options()
            .write(true)
            .open(part)
            .and_then(|file| file.set_modified(when))
            .is_ok()
    {
        return;
    }
    let _ = tokio::fs::remove_file(part).await;
}

/// Forces a file that is already on the Mac to the disk.
async fn force_to_disk(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    SYNCS.with(|syncs| syncs.set(syncs.get() + 1));
    tokio::fs::File::open(path).await?.sync_all().await
}

/// Moves a finished `.part` file to `target`, never replacing anything: a
/// device can list two files with the same name, and something else may
/// have created the name since the plan was made. A taken name gets the
/// next free number. Returns where the file ended up.
pub(super) async fn place(part: &Path, target: &Path) -> std::io::Result<PathBuf> {
    let folder = target.parent().unwrap_or(Path::new("/"));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut n = 0;
    loop {
        let candidate = folder.join(mapper::numbered(&name, n, false));
        // A hard link fails if the name exists, so placing is atomic.
        match tokio::fs::hard_link(part, &candidate).await {
            Ok(()) => {
                tokio::fs::remove_file(part).await?;
                return Ok(candidate);
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => n += 1,
            Err(err) if err.kind() == ErrorKind::NotFound => return Err(err),
            // File systems without hard links (exFAT, FAT): check, then rename.
            Err(_) => match tokio::fs::symlink_metadata(&candidate).await {
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    tokio::fs::rename(part, &candidate).await?;
                    return Ok(candidate);
                }
                _ => n += 1,
            },
        }
    }
}

/// Whether a copy just placed on the Mac is exactly what the phone said the
/// file was: all `reported` bytes arrived (`received`) and the file on disk is
/// that long. A move removes the original only for a copy this vouches for.
/// A phone that gives no size (`0xFFFFFFFF`, 4 GB or more on some phones)
/// cannot be checked at all, and a size that disagrees with what arrived
/// (a stale one, say) is not trusted either: the copy is kept as it is, the
/// original stays.
async fn verify_placed(target: &Path, reported: u64, received: u64) -> bool {
    if !size_is_known(reported) || received != reported {
        return false;
    }
    tokio::fs::metadata(target)
        .await
        .is_ok_and(|found| found.is_file() && found.len() == reported)
}

/// Copies one device file into place.
struct Copy<'a, P> {
    storage: &'a Storage,
    handle: ObjectHandle,
    partial_reads: bool,
    cancel: &'a Cancel,
    progress: &'a P,
    started: Instant,
    /// How long to wait on a quiet phone.
    stall_limit: Duration,
    /// Told whenever a window arrives, so requests queued behind this copy know
    /// the phone is working and not silent.
    heard: device::Activity,
    /// A move: the originals go once the copies are known to be whole.
    moving: bool,
    /// Each file is sent on and deleted from the Mac as soon as it is here (a
    /// copy through the Mac), so the phone's copy is the one that counts and
    /// there is no later try to resume a part for.
    relayed: bool,
}

impl<P> Copy<'_, P> {
    /// Each file is forced to the disk before it counts as copied. Worth its
    /// cost (about 4 ms a file on an SSD, which is 20 s for a camera folder of
    /// 5,000) only when the Mac copy is what protects the data: a move is about
    /// to delete the original. A plain copy leaves the original in place, as
    /// Finder's does without the wait, and a relayed file is thrown away at
    /// once.
    fn durable(&self) -> bool {
        self.moving && !self.relayed
    }

    /// A part left by a failed or cancelled try may be continued later, if the
    /// phone can read from an offset. A relayed file is never tried again from
    /// its part (the scratch folder goes at the end), and kept parts would
    /// pile up in it beside the one file the Mac is meant to hold.
    fn resumable(&self) -> bool {
        self.partial_reads && !self.relayed
    }
}

impl<P: Fn(TransferProgress)> Copy<'_, P> {
    /// Copies the file to `target`, and says whether it carried on from an
    /// earlier attempt's part file and whether the copy was checked.
    async fn to(
        &self,
        target: &Path,
        how: &How,
        modified: Option<DateTime>,
        state: &mut TransferProgress,
    ) -> Result<Copied, Failure> {
        let when = modified.and_then(mapper::to_system_time);
        let carried_on = match how {
            How::Resume { part, offset } => open_resumable(part, *offset)
                .await
                .map(|file| (part.clone(), file, *offset)),
            _ => None,
        };
        let (part, file, offset) = match carried_on {
            Some(opened) => opened,
            None => {
                if let How::Resume { offset, .. } = how {
                    // The part file changed since the plan: start over, and
                    // the extra bytes were not in the totals.
                    state.bytes_total += *offset;
                    state.file_bytes_done = 0;
                }
                let (part, file) = create_part(target)
                    .await
                    .map_err(|err| Failure::Mac(err, target.to_path_buf()))?;
                (part, file, 0)
            }
        };

        let written = self.write(file, &part, offset, state).await;
        if let Err(failure) = written {
            settle_part(&part, &failure, when, self.resumable()).await;
            return Err(failure);
        }

        // Marked before it has its name, so it is never there unmarked. A
        // disk that can't hold the mark (FAT) still gets the file.
        let _ = crate::service::quarantine::mark(&part);
        let target = place(&part, target)
            .await
            .map_err(|err| Failure::Mac(err, target.to_path_buf()))?;
        if let Some(time) = modified.and_then(mapper::to_system_time) {
            // A copy with today's date is still a good copy; the date is a courtesy.
            let _ = std::fs::File::options()
                .write(true)
                .open(&target)
                .and_then(|file| file.set_modified(time));
        }
        let verified = verify_placed(&target, state.file_bytes_total, state.file_bytes_done).await;
        Ok(Copied {
            resumed: offset > 0,
            verified,
        })
    }

    /// Writes the device file from `offset` on into `file`. Whatever stops it,
    /// what was received is flushed to disk first: a `tokio` file drops its
    /// buffered writes otherwise, and the part file kept for resuming would
    /// come up short.
    async fn write(
        &self,
        mut file: tokio::fs::File,
        part: &Path,
        offset: u64,
        state: &mut TransferProgress,
    ) -> Result<(), Failure> {
        let result = self.write_from(&mut file, part, offset, state).await;
        if result.is_err() {
            let _ = file.flush().await;
        }
        result
    }

    async fn write_from(
        &self,
        file: &mut tokio::fs::File,
        part: &Path,
        offset: u64,
        state: &mut TransferProgress,
    ) -> Result<(), Failure> {
        let mac = |err| Failure::Mac(err, part.to_path_buf());
        let limit = size_limit(state.file_bytes_total);

        if self.partial_reads {
            // Windows free the session between reads, so stopping is just not
            // asking for the next one: the documented safe way to cancel.
            let range = if offset > 0 {
                ByteRange::From(offset)
            } else {
                ByteRange::Full
            };
            let mut download = phone_answers(
                &self.heard,
                self.stall_limit,
                self.storage.download_windowed(self.handle, range, WINDOW),
            )
            .await?
            .map_err(Failure::Device)?;
            while let Some(window) =
                phone_answers(&self.heard, self.stall_limit, download.next_window()).await?
            {
                let window = window.map_err(Failure::Device)?;
                check_within_size(state.file_bytes_done, window.len(), limit)?;
                file.write_all(&window).await.map_err(mac)?;
                self.advance(window.len(), state);
                // A file that has fully arrived is just placed; the batch
                // stops before the next one.
                if cancel_stops(self.cancel.is_cancelled(), state.file_bytes_done, limit) {
                    return Err(Failure::Cancelled);
                }
            }
        } else {
            // Devices without ranged reads only send whole files in one go.
            let mut download = phone_answers(
                &self.heard,
                self.stall_limit,
                self.storage.download(self.handle, ByteRange::Full),
            )
            .await?
            .map_err(Failure::Device)?;
            while let Some(chunk) =
                phone_answers(&self.heard, self.stall_limit, download.next_chunk()).await?
            {
                let chunk = chunk.map_err(Failure::Device)?;
                check_within_size(state.file_bytes_done, chunk.len(), limit)?;
                file.write_all(&chunk).await.map_err(mac)?;
                self.advance(chunk.len(), state);
                if cancel_stops(self.cancel.is_cancelled(), state.file_bytes_done, limit) {
                    let _ = download.cancel(mtp_rs::mtp::DEFAULT_CANCEL_TIMEOUT).await;
                    return Err(Failure::Cancelled);
                }
            }
        }

        // Every byte is here; what is left (getting it onto the disk) can take
        // a few seconds on a big file, which should not look like a hang.
        state.finishing = true;
        state.elapsed = self.started.elapsed();
        (self.progress)(state.clone());
        file.flush().await.map_err(mac)?;
        if self.durable() {
            #[cfg(test)]
            SYNCS.with(|syncs| syncs.set(syncs.get() + 1));
            file.sync_all().await.map_err(mac)?;
        }
        Ok(())
    }

    fn advance(&self, bytes: usize, state: &mut TransferProgress) {
        let bytes = bytes as u64;
        state.file_bytes_done += bytes;
        state.bytes_done += bytes;
        state.elapsed = self.started.elapsed();
        self.heard.heard();
        (self.progress)(state.clone());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Mutex;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::service::testing::virtual_phone;

    struct Setup {
        phone: tempfile::TempDir,
        mac: tempfile::TempDir,
        service: Service,
        storage_id: u64,
    }

    /// A phone with DCIM/Camera/{a.jpg, b.jpg}, notes.txt and an empty
    /// Music folder, and an empty Mac folder to copy into.
    async fn setup_with(limit: impl FnOnce(&mut device::VirtualPhone)) -> Setup {
        let phone = tempfile::tempdir().unwrap();
        fs::create_dir_all(phone.path().join("DCIM/Camera")).unwrap();
        fs::create_dir_all(phone.path().join("Music")).unwrap();
        fs::write(phone.path().join("DCIM/Camera/a.jpg"), b"aaaa").unwrap();
        fs::write(phone.path().join("DCIM/Camera/b.jpg"), b"bb").unwrap();
        fs::write(phone.path().join("notes.txt"), b"hello").unwrap();

        let mut config = virtual_phone(phone.path());
        limit(&mut config);
        let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));

        Setup {
            phone,
            mac: tempfile::tempdir().unwrap(),
            service: Service::new(device, explorer),
            storage_id,
        }
    }

    async fn setup() -> Setup {
        setup_with(|_| {}).await
    }

    impl Setup {
        fn request(&self, source: &str, names: &[&str]) -> DownloadRequest {
            DownloadRequest {
                storage_id: self.storage_id,
                source: source.into(),
                names: names.iter().map(|n| (*n).to_owned()).collect(),
                destination: self.mac.path().to_string_lossy().into_owned(),
            }
        }

        fn mac_names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.mac.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    /// Records every progress report.
    #[derive(Default)]
    struct Log(Mutex<Vec<TransferProgress>>);

    impl Log {
        fn record(&self, progress: TransferProgress) {
            self.0.lock().unwrap().push(progress);
        }

        fn take(self) -> Vec<TransferProgress> {
            self.0.into_inner().unwrap()
        }
    }

    #[tokio::test]
    async fn downloads_a_file_with_its_bytes() {
        let s = setup().await;

        let summary = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
        assert_eq!(summary.files, 1);
        assert_eq!(summary.bytes, 5);
        assert_eq!(summary.renamed, 0);
        assert_eq!(summary.first_item.as_deref(), Some("notes.txt"));
    }

    #[tokio::test]
    async fn every_file_from_the_phone_is_marked_as_come_from_elsewhere() {
        let s = setup().await;

        s.service
            .download(
                &s.request("/", &["notes.txt", "DCIM"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        for file in ["notes.txt", "DCIM/Camera/a.jpg"] {
            let mark = crate::service::quarantine::read(&s.mac.path().join(file));
            assert!(
                mark.is_some_and(|mark| mark.ends_with(";open-mtp-client;")),
                "{file}: the Mac checks it before it runs"
            );
        }
    }

    #[tokio::test]
    async fn the_waits_around_a_copy_count_on_the_devices_own_clock() {
        // Planning and clearing a name wait on a phone busy with something
        // else (an explorer request, another transfer) only if they count
        // what it does; on a clock of their own they would give up early.
        let s = setup().await;

        assert!(s.service.limits.counts_on(&s.service.device.activity()));
    }

    #[tokio::test]
    async fn every_window_received_tells_the_session_the_phone_is_alive() {
        // Requests queued behind a long download judge the phone by this:
        // without it they would call a busy phone silent.
        let s = setup().await;
        let big = s.phone.path().join("big.bin");
        fs::write(&big, vec![7u8; 5 * 1024 * 1024]).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);
        let activity = s.service.device.activity();
        let heard = Mutex::new(Vec::new());

        s.service
            .download(&s.request("/", &["big.bin"]), &Cancel::new(), |p| {
                if p.file_bytes_done > 0 && !p.finishing {
                    heard.lock().unwrap().push(activity.times_heard());
                }
            })
            .await
            .unwrap();

        let heard = heard.into_inner().unwrap();
        assert!(heard.len() >= 2, "several windows came over: {heard:?}");
        assert!(
            heard.windows(2).all(|pair| pair[0] < pair[1]),
            "each window was reported: {heard:?}"
        );
    }

    /// An onward stage that notes what it is given and refuses what it is told
    /// to, as a phone that will not take one file or one folder would.
    #[derive(Default)]
    struct Recorder {
        /// "folder x" and "file x/y", in order.
        log: Vec<String>,
        /// What was on the Mac when each file was handed over.
        sizes: Vec<u64>,
        refuse_files: Vec<&'static str>,
        refuse_folders: Vec<&'static str>,
        /// Refuses with a failure that is not about one file.
        lose_the_phone: bool,
    }

    impl Onward for Recorder {
        fn has(&mut self, _: &Path, _: u64, _: Option<SystemTime>) -> bool {
            false
        }

        async fn folder(&mut self, relative: &Path) -> Result<(), Failure> {
            let name = relative.to_string_lossy().into_owned();
            self.log.push(format!("folder {name}"));
            if self.refuse_folders.contains(&name.as_str()) {
                return Err(Failure::Device(mtp_rs::Error::NotFound));
            }
            Ok(())
        }

        async fn file(&mut self, relative: &Path, arrived: &Path) -> Result<(), Failure> {
            let name = relative.to_string_lossy().into_owned();
            self.sizes.push(fs::metadata(arrived).unwrap().len());
            self.log.push(format!("file {name}"));
            if self.lose_the_phone {
                return Err(Failure::Device(mtp_rs::Error::Timeout));
            }
            if self.refuse_files.contains(&name.as_str()) {
                return Err(Failure::Device(mtp_rs::Error::NotFound));
            }
            Ok(())
        }
    }

    async fn onward_run(
        s: &Setup,
        onward: &mut Recorder,
        source: &str,
        names: &[&str],
    ) -> Result<Downloaded, Error> {
        s.service
            .download_run_onward(
                false,
                &s.request(source, names),
                &Cancel::new(),
                &|_| {},
                Some(onward),
            )
            .await
    }

    #[tokio::test]
    async fn each_file_is_handed_on_as_it_arrives_and_is_gone_from_the_mac_after() {
        let s = setup().await;
        let mut onward = Recorder::default();

        let done = onward_run(&s, &mut onward, "/DCIM", &["Camera"])
            .await
            .unwrap();

        assert_eq!(onward.log[0], "folder Camera");
        let mut files = onward.log[1..].to_vec();
        files.sort();
        assert_eq!(files, ["file Camera/a.jpg", "file Camera/b.jpg"]);
        onward.sizes.sort_unstable();
        assert_eq!(onward.sizes, [2, 4], "whole when handed over");
        assert_eq!(done.summary.files, 2);
        assert!(!s.mac.path().join("Camera/a.jpg").exists());
        assert!(!s.mac.path().join("Camera/b.jpg").exists());
        assert!(
            s.mac.path().join("Camera").is_dir(),
            "only the folder is left"
        );
    }

    #[tokio::test]
    async fn a_file_the_onward_stage_refuses_is_that_file_s_failure_and_the_rest_go_on() {
        let s = setup().await;
        let mut onward = Recorder {
            refuse_files: vec!["Camera/a.jpg"],
            ..Recorder::default()
        };

        let done = onward_run(&s, &mut onward, "/DCIM", &["Camera"])
            .await
            .unwrap();

        assert_eq!(done.summary.files, 1);
        assert_eq!(done.summary.failures.len(), 1);
        assert_eq!(done.summary.failures[0].label, "Camera/a.jpg");
        assert!(onward.log.contains(&"file Camera/b.jpg".to_owned()));
        assert!(
            !s.mac.path().join("Camera/a.jpg").exists(),
            "not kept either"
        );
        assert_eq!(
            done.outcomes
                .iter()
                .filter(|o| **o == Outcome::Verified)
                .count(),
            2,
            "the folder and b.jpg; a.jpg did not go, so its original stays"
        );
    }

    #[tokio::test]
    async fn a_folder_the_onward_stage_cannot_make_takes_what_is_in_it_with_it() {
        let s = setup().await;
        let mut onward = Recorder {
            refuse_folders: vec!["DCIM/Camera"],
            ..Recorder::default()
        };

        let result = onward_run(&s, &mut onward, "/", &["DCIM", "notes.txt"]).await;

        let done = result.unwrap();
        assert!(
            !onward
                .log
                .iter()
                .any(|entry| entry.starts_with("file DCIM/Camera")),
            "nothing goes into a folder that was not made: {:?}",
            onward.log
        );
        assert!(onward.log.contains(&"file notes.txt".to_owned()));
        assert_eq!(done.summary.files, 1);
        assert_eq!(done.summary.failures.len(), 1);
    }

    #[tokio::test]
    async fn an_onward_stage_that_loses_the_phone_ends_the_batch() {
        let s = setup().await;
        let mut onward = Recorder {
            lose_the_phone: true,
            ..Recorder::default()
        };

        let result = onward_run(&s, &mut onward, "/DCIM", &["Camera"]).await;

        assert_eq!(result.err(), Some(Error::Timeout));
        assert_eq!(onward.log.len(), 2, "a folder and the one file, no more");
        assert!(!s.mac.path().join("Camera/a.jpg").exists());
    }

    fn files_forced_to_disk() -> usize {
        SYNCS.with(std::cell::Cell::get)
    }

    #[tokio::test]
    async fn a_plain_copy_does_not_wait_for_each_file_to_reach_the_disk() {
        // About 4 ms a file on an SSD: 20 s for a camera folder of 5,000, for
        // files whose originals are still on the phone.
        let s = setup().await;
        let before = files_forced_to_disk();

        let summary = s
            .service
            .download(&s.request("/DCIM", &["Camera"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.files, 2);
        assert_eq!(files_forced_to_disk(), before);
    }

    #[tokio::test]
    async fn a_move_that_finds_its_copy_already_there_forces_it_to_disk_before_the_original_goes() {
        // A plain copy does not wait for the disk, so a copy found in place may
        // not be there if the Mac lost power since; the move waits for it now.
        let s = setup().await;
        s.service
            .download(&s.request("/DCIM", &["Camera"]), &Cancel::new(), |_| {})
            .await
            .unwrap();
        let before = files_forced_to_disk();

        let summary = s
            .service
            .download_as(
                Paste::Move,
                &s.request("/DCIM", &["Camera"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(summary.already_there, 2, "nothing was copied again");
        assert_eq!(files_forced_to_disk() - before, 2, "but both were forced");
        assert!(!s.phone.path().join("DCIM/Camera/a.jpg").exists());
    }

    #[tokio::test]
    async fn a_copy_that_finds_its_files_already_there_does_not_wait_for_the_disk_either() {
        let s = setup().await;
        let request = s.request("/DCIM", &["Camera"]);
        s.service
            .download(&request, &Cancel::new(), |_| {})
            .await
            .unwrap();
        let before = files_forced_to_disk();

        let summary = s
            .service
            .download(&request, &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.already_there, 2);
        assert_eq!(files_forced_to_disk(), before);
        let arrival = summary.arrival.unwrap();
        assert_eq!(
            (arrival.files, arrival.there, arrival.checked),
            (2, 2, Some(2)),
            "nothing copied, and still all there"
        );
    }

    #[test]
    fn the_report_says_what_became_of_each_file_and_of_its_original() {
        let file = |label: &str, how: How| Step::File {
            handle: ObjectHandle(1),
            target: PathBuf::from("/x"),
            label: label.into(),
            size: 10,
            modified: None,
            how,
        };
        let origin = |name: &str| Origin {
            handle: ObjectHandle(1),
            parent: String::new(),
            name: name.into(),
            size: 10,
            modified: None,
            is_folder: false,
            top: 0,
            is_top: true,
        };
        let resume = How::Resume {
            part: PathBuf::from("/p"),
            offset: 3,
        };
        let plan = Plan {
            steps: vec![
                Step::Folder(PathBuf::from("/d")),
                file("a (1).jpg", How::Fresh),
                file("b.jpg", How::Fresh),
                file("c.jpg", resume),
                file("d.jpg", How::AlreadyThere),
                file("e.jpg", How::Fresh),
                file("f.jpg", How::Fresh),
                file("g.jpg", How::Fresh),
                file("h.jpg", How::Fresh),
            ],
            origins: [
                "d", "a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg", "f.jpg", "g.jpg", "h.jpg",
            ]
            .map(origin)
            .to_vec(),
            items: Vec::new(),
            unwalked: false,
            renamed: 1,
            first_item: None,
            replaces: vec![
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(PathBuf::from("/x/g.jpg")),
                Some(PathBuf::from("/x/h.jpg")),
            ],
            skipped: vec!["Trip".into()],
        };
        let outcomes = [
            Outcome::Verified,
            Outcome::Verified,
            Outcome::Unchecked,
            Outcome::Verified,
            Outcome::Verified,
            Outcome::NotCopied,
            Outcome::NotCopied,
            Outcome::Verified,
            Outcome::Unchecked,
        ];
        let failures = [FileFailure {
            label: "e.jpg".into(),
            error: Error::MacDiskFull,
        }];

        let (listed, left_out) = report(&plan, &outcomes, &failures, None);

        assert_eq!(left_out, 0);
        let outcome = |at: usize| listed[at].outcome.clone();
        assert_eq!(listed.len(), 9, "eight files and the item skipped");
        assert_eq!(listed[0].path, "a (1).jpg");
        assert_eq!(listed[0].from.as_deref(), Some("a.jpg"), "saved as");
        assert_eq!(listed[1].from, None);
        assert_eq!(outcome(0), FileOutcome::Copied { checked: true });
        assert_eq!(outcome(1), FileOutcome::Copied { checked: false });
        assert_eq!(outcome(2), FileOutcome::Resumed { checked: true });
        assert_eq!(outcome(3), FileOutcome::AlreadyThere);
        assert_eq!(outcome(4), FileOutcome::Failed(Error::MacDiskFull));
        assert_eq!(outcome(5), FileOutcome::NotCopied, "no failure to say why");
        assert_eq!(outcome(6), FileOutcome::Replaced);
        assert_eq!(
            outcome(7),
            FileOutcome::NotCopied,
            "not whole: not swapped in"
        );
        assert_eq!(
            (listed[8].path.as_str(), outcome(8)),
            ("Trip", FileOutcome::Skipped)
        );
        assert!(listed.iter().all(|file| file.original.is_none()), "a copy");

        // A move: each original's fate, unnoted ones stayed.
        let mut fates = vec![None; plan.steps.len()];
        note_fate(&mut fates, 1, Original::Removed);
        note_fate(&mut fates, 2, Original::Kept(Why::Unchecked));
        note_fate(&mut fates, 99, Original::Removed);
        let (moved, _) = report(&plan, &outcomes, &failures, Some(&fates));
        assert_eq!(moved[0].original, Some(Original::Removed));
        assert_eq!(moved[1].original, Some(Original::Kept(Why::Unchecked)));
        assert_eq!(moved[4].original, Some(Original::Stayed));
        assert_eq!(moved[8].original, Some(Original::Stayed), "skipped stays");
    }

    #[test]
    fn a_report_lists_the_first_hundred_thousand_files_and_counts_the_rest() {
        let file = || Step::File {
            handle: ObjectHandle(1),
            target: PathBuf::from("/x"),
            label: "x".into(),
            size: 1,
            modified: None,
            how: How::Fresh,
        };
        let plan = Plan {
            steps: (0..REPORT_MOST + 5).map(|_| file()).collect(),
            origins: Vec::new(),
            items: Vec::new(),
            unwalked: false,
            renamed: 0,
            first_item: None,
            replaces: Vec::new(),
            skipped: vec!["Trip".into()],
        };
        let outcomes = vec![Outcome::Verified; REPORT_MOST + 5];

        let (listed, left_out) = report(&plan, &outcomes, &[], None);

        assert_eq!(listed.len(), REPORT_MOST);
        assert_eq!(left_out, 6, "five files and the skipped item");
    }

    #[test]
    fn the_count_says_what_arrived_what_was_checked_and_whether_all_was_seen() {
        let file = |size: u64| Step::File {
            handle: ObjectHandle(1),
            target: PathBuf::from("/x"),
            label: "x".into(),
            size,
            modified: None,
            how: How::Fresh,
        };
        let plan = |unwalked: bool| Plan {
            steps: vec![
                Step::Folder(PathBuf::from("/d")),
                file(10),
                file(20),
                file(0xFFFF_FFFF),
                file(40),
            ],
            origins: Vec::new(),
            items: Vec::new(),
            unwalked,
            renamed: 0,
            first_item: None,
            replaces: Vec::new(),
            skipped: Vec::new(),
        };
        let outcomes = [
            Outcome::Verified,
            Outcome::Verified,
            Outcome::Unchecked,
            Outcome::Verified,
            Outcome::NotCopied,
        ];

        assert_eq!(
            arrival(&plan(false), &outcomes),
            crate::dto::Arrival {
                files: 4,
                bytes: 70,
                there: 3,
                checked: Some(2),
                complete: true,
            },
            "the folder is not a file; a size of 4 GB or more counts no bytes"
        );
        assert!(!arrival(&plan(true), &outcomes).complete);
    }

    /// Copies a 5 MiB file from the phone with `relayed` as given, cancelling
    /// after the first window, and says whether its part file is still there.
    async fn part_left_after_a_cancel(relayed: bool) -> bool {
        let s = setup().await;
        fs::write(s.phone.path().join("big.bin"), vec![3u8; 5 * 1024 * 1024]).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);
        let cancel = Cancel::new();
        let log = Log::default();
        let copy = Copy {
            storage: &s
                .service
                .device
                .session()
                .await
                .unwrap()
                .storage(mtp_rs::StorageId(s.storage_id))
                .await
                .unwrap(),
            handle: handle_of(&s, "big.bin").await,
            partial_reads: true,
            cancel: &cancel,
            progress: &|p: TransferProgress| {
                if p.file_bytes_done > 0 {
                    cancel.cancel();
                }
                log.record(p);
            },
            started: Instant::now(),
            stall_limit: STALL_LIMIT,
            heard: s.service.device.activity(),
            moving: false,
            relayed,
        };
        let mut state = TransferProgress {
            bytes_total: 5 * 1024 * 1024,
            file_bytes_total: 5 * 1024 * 1024,
            ..TransferProgress::default()
        };
        let when = mapper::to_device_time(UNIX_EPOCH + Duration::from_secs(1_791_376_200));

        let result = copy
            .to(&s.mac.path().join("big.bin"), &How::Fresh, when, &mut state)
            .await;

        assert!(matches!(result, Err(Failure::Cancelled)));
        s.mac.path().join(".big.bin.open-mtp-client-part").exists()
    }

    #[tokio::test]
    async fn a_part_is_kept_for_a_later_try_unless_the_file_is_on_its_way_to_the_phone() {
        assert!(
            part_left_after_a_cancel(false).await,
            "a plain copy keeps it to continue from"
        );
        assert!(
            !part_left_after_a_cancel(true).await,
            "a relayed file's scratch folder goes at the end: nothing will continue it"
        );
    }

    #[tokio::test]
    async fn a_move_to_the_mac_forces_each_file_to_disk_before_its_original_goes() {
        let s = setup().await;
        let before = files_forced_to_disk();

        let summary = s
            .service
            .download_as(
                Paste::Move,
                &s.request("/DCIM", &["Camera"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(summary.files, 2);
        assert_eq!(files_forced_to_disk() - before, 2, "one for each file");
        assert!(!s.phone.path().join("DCIM/Camera/a.jpg").exists());
    }

    #[tokio::test]
    async fn downloads_a_folder_with_everything_in_it() {
        let s = setup().await;

        let summary = s
            .service
            .download(&s.request("/", &["DCIM", "Music"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(
            fs::read(s.mac.path().join("DCIM/Camera/a.jpg")).unwrap(),
            b"aaaa"
        );
        assert_eq!(
            fs::read(s.mac.path().join("DCIM/Camera/b.jpg")).unwrap(),
            b"bb"
        );
        assert!(s.mac.path().join("Music").is_dir());
        assert_eq!((summary.files, summary.bytes), (2, 6));
        assert_eq!(
            summary.arrival,
            Some(crate::dto::Arrival {
                files: 2,
                bytes: 6,
                there: 2,
                checked: Some(2),
                complete: true,
            }),
            "both files, each the size the phone gave"
        );
    }

    #[tokio::test]
    async fn progress_counts_files_and_bytes_up_to_the_totals() {
        let s = setup().await;
        let log = Log::default();

        s.service
            .download(
                &s.request("/", &["DCIM", "notes.txt"]),
                &Cancel::new(),
                |p| log.record(p),
            )
            .await
            .unwrap();

        let log = log.take();
        let last = log.last().unwrap();
        assert_eq!((last.files_done, last.files_total), (3, 3));
        assert_eq!((last.bytes_done, last.bytes_total), (11, 11));
        assert!(log.iter().any(|p| p.current_file == "DCIM/Camera/a.jpg"));
        assert!(log.windows(2).all(|w| w[0].bytes_done <= w[1].bytes_done));
    }

    #[tokio::test]
    async fn a_large_file_reports_progress_per_window() {
        let s = setup().await;
        fs::write(s.phone.path().join("video.mp4"), vec![7u8; 5 * 1024 * 1024]).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);
        let log = Log::default();

        s.service
            .download(&s.request("/", &["video.mp4"]), &Cancel::new(), |p| {
                log.record(p)
            })
            .await
            .unwrap();

        let chunks: Vec<u64> = log
            .take()
            .iter()
            .map(|p| p.file_bytes_done)
            .filter(|done| *done > 0)
            .collect();
        assert!(
            chunks.len() >= 3,
            "expected several windows, got {chunks:?}"
        );
        assert_eq!(
            fs::metadata(s.mac.path().join("video.mp4")).unwrap().len(),
            5 * 1024 * 1024
        );
    }

    #[tokio::test]
    async fn a_different_file_is_kept_and_a_folder_is_merged_into() {
        let s = setup().await;
        fs::write(s.mac.path().join("notes.txt"), b"mine").unwrap();
        fs::create_dir(s.mac.path().join("DCIM")).unwrap();
        fs::write(s.mac.path().join("DCIM/mine.jpg"), b"keep").unwrap();

        let summary = s
            .service
            .download(
                &s.request("/", &["notes.txt", "DCIM"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"mine");
        assert_eq!(
            fs::read(s.mac.path().join("notes (1).txt")).unwrap(),
            b"hello"
        );
        assert_eq!(
            fs::read(s.mac.path().join("DCIM/mine.jpg")).unwrap(),
            b"keep"
        );
        assert!(s.mac.path().join("DCIM/Camera/a.jpg").exists());
        assert!(
            !s.mac.path().join("DCIM (1)").exists(),
            "merged, not copied beside"
        );
        assert_eq!(summary.renamed, 1, "only the file needed a new name");
        assert_eq!(summary.first_item.as_deref(), Some("notes (1).txt"));
    }
    #[tokio::test]
    async fn files_keep_the_phones_modified_date() {
        let s = setup().await;
        let when = UNIX_EPOCH + Duration::from_secs(1_791_376_200);
        fs::File::options()
            .write(true)
            .open(s.phone.path().join("notes.txt"))
            .unwrap()
            .set_modified(when)
            .unwrap();

        s.service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        let copied = fs::metadata(s.mac.path().join("notes.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(copied, when);
    }

    #[tokio::test]
    async fn cancelling_between_files_keeps_the_finished_ones_and_starts_no_more() {
        let s = setup().await;
        let cancel = Cancel::new();
        let trigger = cancel.clone();

        let result = s
            .service
            .download(
                &s.request("/DCIM/Camera", &["a.jpg", "b.jpg"]),
                &cancel,
                |p| {
                    // The first file has fully arrived.
                    if p.file_bytes_total > 0 && p.file_bytes_done == p.file_bytes_total {
                        trigger.cancel();
                    }
                },
            )
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        assert_eq!(
            s.mac_names(),
            ["a.jpg"],
            "a.jpg is placed, b.jpg never starts"
        );
    }
    #[tokio::test]
    async fn a_cancel_before_starting_copies_nothing() {
        let s = setup().await;
        let cancel = Cancel::new();
        cancel.cancel();

        let result = s
            .service
            .download(&s.request("/", &["notes.txt"]), &cancel, |_| {})
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        assert!(s.mac_names().is_empty());
    }

    #[tokio::test]
    async fn a_device_without_ranged_reads_still_downloads() {
        let s = setup_with(|phone| phone.partial_reads = false).await;

        s.service
            .download(&s.request("/DCIM", &["Camera"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(
            fs::read(s.mac.path().join("Camera/a.jpg")).unwrap(),
            b"aaaa"
        );
    }

    #[tokio::test]
    async fn cancel_also_works_without_ranged_reads() {
        let s = setup_with(|phone| phone.partial_reads = false).await;
        let cancel = Cancel::new();
        let trigger = cancel.clone();

        let result = s
            .service
            .download(
                &s.request("/DCIM/Camera", &["a.jpg", "b.jpg"]),
                &cancel,
                |p| {
                    if p.file_bytes_done > 0 {
                        trigger.cancel();
                    }
                },
            )
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        // The phone sends a file in one go, so the first one lands; the batch
        // stops before the second, and no part file is left (without ranged
        // reads there is nothing to continue from).
        assert_eq!(s.mac_names(), ["a.jpg"]);
    }
    #[tokio::test]
    async fn a_missing_name_copies_nothing() {
        let s = setup().await;

        let result = s
            .service
            .download(
                &s.request("/", &["notes.txt", "ghost"]),
                &Cancel::new(),
                |_| {},
            )
            .await;

        assert_eq!(result, Err(Error::PathNotFound("/ghost".into())));
        assert!(s.mac_names().is_empty());
    }

    #[tokio::test]
    async fn a_missing_destination_is_path_not_found() {
        let s = setup().await;
        let mut request = s.request("/", &["notes.txt"]);
        request.destination = s.mac.path().join("nope").to_string_lossy().into_owned();

        let result = s.service.download(&request, &Cancel::new(), |_| {}).await;

        assert_eq!(result, Err(Error::PathNotFound(request.destination)));
    }

    #[tokio::test]
    async fn a_file_as_destination_is_path_not_found() {
        let s = setup().await;
        fs::write(s.mac.path().join("file"), b"x").unwrap();
        let mut request = s.request("/", &["notes.txt"]);
        request.destination = s.mac.path().join("file").to_string_lossy().into_owned();

        let result = s.service.download(&request, &Cancel::new(), |_| {}).await;

        assert_eq!(result, Err(Error::PathNotFound(request.destination)));
    }

    #[tokio::test]
    async fn an_unwritable_destination_is_access_denied() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup().await;
        let locked = s.mac.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let mut request = s.request("/", &["notes.txt"]);
        request.destination = locked.to_string_lossy().into_owned();

        let result = s.service.download(&request, &Cancel::new(), |_| {}).await;

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(Error::FolderAccessDenied(_))));
    }

    /// A fake device tree: folder handle → children.
    struct Tree(HashMap<u64, Vec<Node>>);

    impl Children for Tree {
        async fn children(&self, folder: ObjectHandle) -> Result<Vec<Node>, mtp_rs::Error> {
            Ok(self.0.get(&folder.0).cloned().unwrap_or_default())
        }
    }

    fn node(handle: u64, name: &str, is_folder: bool) -> Node {
        Node {
            handle: ObjectHandle(handle),
            name: name.into(),
            is_folder,
            size: 1,
            modified: None,
        }
    }

    fn tree(entries: &[(u64, Vec<Node>)]) -> Tree {
        Tree(entries.iter().cloned().collect())
    }

    #[tokio::test]
    async fn a_target_behind_a_file_is_decided_at_once_not_numbered_for_ever() {
        let mac = tempfile::tempdir().unwrap();
        std::fs::write(mac.path().join("in the way"), b"x").unwrap();
        let base = mac.path().join("in the way/a.jpg");

        let decided = tokio::time::timeout(
            Duration::from_secs(5),
            resolve_file(&base, 1, None, true, &HashSet::new()),
        )
        .await
        .expect("decided");

        assert_eq!(decided, (base, How::Fresh));
    }

    #[tokio::test]
    async fn plan_lists_folders_before_their_files() {
        let mac = tempfile::tempdir().unwrap();
        let device = tree(&[(1, vec![node(2, "a.jpg", false)])]);

        let plan = plan(&device, &[node(1, "DCIM", true)], mac.path(), true)
            .await
            .unwrap();

        assert!(matches!(&plan.steps[0], Step::Folder(p) if p == &mac.path().join("DCIM")));
        assert!(matches!(&plan.steps[1], Step::File { label, .. } if label == "DCIM/a.jpg"));
    }

    #[tokio::test]
    async fn two_phone_names_differing_only_in_case_replace_one_mac_file_once() {
        let mac = tempfile::tempdir().unwrap();
        std::fs::write(mac.path().join("notes.txt"), b"mac").unwrap();
        let device = tree(&[]);

        let plan = plan_with(
            &device,
            &[node(1, "notes.txt", false), node(2, "NOTES.TXT", false)],
            mac.path(),
            true,
            Conflict::Replace,
        )
        .await
        .unwrap();

        let replacing: Vec<_> = plan.replaces.iter().flatten().collect();
        assert_eq!(replacing, [&mac.path().join("notes.txt")], "only the first");
        assert_eq!(
            plan.skipped,
            ["NOTES.TXT"],
            "the second is left out, not swapped in"
        );
        assert_eq!(plan.steps.len(), 1);
    }

    #[tokio::test]
    async fn a_child_named_to_escape_stops_the_plan() {
        let mac = tempfile::tempdir().unwrap();
        let device = tree(&[(1, vec![node(2, "../../.zshrc", false)])]);

        let result = plan(&device, &[node(1, "DCIM", true)], mac.path(), true).await;

        assert!(matches!(result, Err(Failure::UnsafeName(n)) if n == "../../.zshrc"));
    }

    #[tokio::test]
    async fn a_selected_item_with_an_absolute_name_stops_the_plan() {
        let mac = tempfile::tempdir().unwrap();

        let result = plan(
            &tree(&[]),
            &[node(1, "/etc/passwd", false)],
            mac.path(),
            true,
        )
        .await;

        assert!(matches!(result, Err(Failure::UnsafeName(_))));
    }

    #[tokio::test]
    async fn a_folder_that_contains_itself_is_walked_once() {
        let mac = tempfile::tempdir().unwrap();
        // 1 = Loop { 2 = a.jpg, 1 = Loop again }
        let device = tree(&[(1, vec![node(2, "a.jpg", false), node(1, "Loop", true)])]);

        let plan = plan(&device, &[node(1, "Loop", true)], mac.path(), true)
            .await
            .unwrap();

        let files = plan
            .steps
            .iter()
            .filter(|step| matches!(step, Step::File { .. }))
            .count();
        assert_eq!(files, 1);
    }

    #[tokio::test]
    async fn the_walk_stops_at_the_depth_limit() {
        let mac = tempfile::tempdir().unwrap();
        // 1 → 2 → 3 → 4, each a folder inside the last.
        let device = tree(&[
            (1, vec![node(2, "b", true)]),
            (2, vec![node(3, "c", true)]),
            (3, vec![node(4, "d", true)]),
        ]);
        let mut walker = Walker {
            conflict: Conflict::KeepBoth,
            replaces: Vec::new(),
            replacing: HashSet::new(),
            skipped: Vec::new(),
            device: &device,
            destination: mac.path(),
            steps: Vec::new(),
            origins: Vec::new(),
            phone_parent: String::new(),
            top: 0,
            unwalked: false,
            visited: HashSet::new(),
            max_depth: 2,
            max_items: MAX_ITEMS,
            partial_reads: true,
        };

        let result = walker.walk(&node(1, "a", true), mac.path(), "", 0).await;

        assert!(matches!(result, Err(Failure::TooLarge)));
    }

    #[tokio::test]
    async fn the_walk_stops_at_the_item_limit() {
        let mac = tempfile::tempdir().unwrap();
        let device = tree(&[(
            1,
            (2..10)
                .map(|h| node(h, &format!("{h}.jpg"), false))
                .collect(),
        )]);
        let mut walker = Walker {
            conflict: Conflict::KeepBoth,
            replaces: Vec::new(),
            replacing: HashSet::new(),
            skipped: Vec::new(),
            device: &device,
            destination: mac.path(),
            steps: Vec::new(),
            origins: Vec::new(),
            phone_parent: String::new(),
            top: 0,
            unwalked: false,
            visited: HashSet::new(),
            max_depth: MAX_DEPTH,
            max_items: 3,
            partial_reads: true,
        };

        let result = walker.walk(&node(1, "a", true), mac.path(), "", 0).await;

        assert!(matches!(result, Err(Failure::TooLarge)));
    }

    #[tokio::test]
    async fn a_target_outside_the_destination_is_refused() {
        let mac = tempfile::tempdir().unwrap();
        let mut walker = Walker {
            conflict: Conflict::KeepBoth,
            replaces: Vec::new(),
            replacing: HashSet::new(),
            skipped: Vec::new(),
            device: &tree(&[]),
            destination: mac.path(),
            steps: Vec::new(),
            origins: Vec::new(),
            phone_parent: String::new(),
            top: 0,
            unwalked: false,
            visited: HashSet::new(),
            max_depth: MAX_DEPTH,
            max_items: MAX_ITEMS,
            partial_reads: true,
        };

        let result = walker
            .walk(&node(1, "x", false), Path::new("/tmp"), "", 0)
            .await;

        assert!(matches!(result, Err(Failure::UnsafeName(_))));
    }

    #[tokio::test]
    async fn placing_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(".a.jpg.open-mtp-client-part");
        fs::write(&part, b"new").unwrap();
        fs::write(dir.path().join("a.jpg"), b"old").unwrap();

        let placed = place(&part, &dir.path().join("a.jpg")).await.unwrap();

        assert_eq!(placed, dir.path().join("a (1).jpg"));
        assert_eq!(fs::read(dir.path().join("a.jpg")).unwrap(), b"old");
        assert_eq!(fs::read(&placed).unwrap(), b"new");
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn placing_into_a_free_name_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(".a.jpg.open-mtp-client-part");
        fs::write(&part, b"new").unwrap();

        let placed = place(&part, &dir.path().join("a.jpg")).await.unwrap();

        assert_eq!(placed, dir.path().join("a.jpg"));
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn placing_a_missing_part_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();

        let result = place(
            &dir.path().join(".gone.open-mtp-client-part"),
            &dir.path().join("a.jpg"),
        )
        .await;

        assert!(result.is_err());
        assert!(!dir.path().join("a.jpg").exists());
    }

    #[tokio::test]
    async fn a_part_name_that_is_taken_is_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.jpg");
        fs::write(
            dir.path().join(".a.jpg.open-mtp-client-part"),
            b"someone else's",
        )
        .unwrap();

        let (part, _file) = create_part(&target).await.unwrap();

        assert_eq!(part, dir.path().join(".a.jpg.1.open-mtp-client-part"));
        assert_eq!(
            fs::read(dir.path().join(".a.jpg.open-mtp-client-part")).unwrap(),
            b"someone else's"
        );
    }

    #[tokio::test]
    async fn a_symlink_at_the_part_name_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"precious").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join(".a.jpg.open-mtp-client-part"))
            .unwrap();

        let (part, _file) = create_part(&dir.path().join("a.jpg")).await.unwrap();

        assert_ne!(part, dir.path().join(".a.jpg.open-mtp-client-part"));
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn a_download_next_to_a_planted_link_leaves_its_target_alone() {
        let s = setup().await;
        let victim = s.mac.path().join("victim.txt");
        fs::write(&victim, b"precious").unwrap();
        std::os::unix::fs::symlink(
            &victim,
            s.mac.path().join(".notes.txt.open-mtp-client-part"),
        )
        .unwrap();

        s.service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
    }

    const VIDEO: usize = 5 * 1024 * 1024;
    const PART: &str = ".video.mp4.open-mtp-client-part";

    /// A 5 MB file on the phone, three windows long, whose bytes say where they are.
    fn video_bytes() -> Vec<u8> {
        (0..VIDEO).map(|i| (i % 251) as u8).collect()
    }

    async fn put_video(s: &Setup) {
        fs::write(s.phone.path().join("video.mp4"), video_bytes()).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);
    }

    /// The phone's date for a file: whole seconds, as phones keep them.
    fn phone_date(s: &Setup, name: &str) -> SystemTime {
        let modified = fs::metadata(s.phone.path().join(name))
            .unwrap()
            .modified()
            .unwrap();
        let secs = modified.duration_since(UNIX_EPOCH).unwrap().as_secs();
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn set_date(path: &Path, when: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// Starts downloading the video and cancels after its first window.
    async fn cancel_the_video(s: &Setup) {
        let cancel = Cancel::new();
        let trigger = cancel.clone();
        let result = s
            .service
            .download(&s.request("/", &["video.mp4"]), &cancel, |p| {
                if p.file_bytes_done > 0 && p.file_bytes_done < p.file_bytes_total {
                    trigger.cancel();
                }
            })
            .await;
        assert_eq!(result, Err(Error::Cancelled));
    }

    #[tokio::test]
    async fn cancelling_mid_file_keeps_what_arrived_to_continue_from() {
        let s = setup().await;
        put_video(&s).await;

        cancel_the_video(&s).await;

        assert_eq!(s.mac_names(), [PART], "no half file under the real name");
        let kept = fs::read(s.mac.path().join(PART)).unwrap();
        assert!(!kept.is_empty() && kept.len() < VIDEO);
        assert_eq!(
            kept,
            video_bytes()[..kept.len()],
            "what is there is the start of the file"
        );
        // Stamped with the phone's date: that is how the next attempt knows
        // it belongs to this file.
        let stamped = fs::metadata(s.mac.path().join(PART))
            .unwrap()
            .modified()
            .unwrap();
        assert!(mapper::same_moment(stamped, phone_date(&s, "video.mp4")));
    }

    #[tokio::test]
    async fn a_download_kept_through_a_quit_continues_from_its_part_after_relaunch() {
        let s = setup().await;
        put_video(&s).await;
        cancel_the_video(&s).await;
        let kept = fs::metadata(s.mac.path().join(PART)).unwrap().len();
        // The app quits with it unfinished; the queue is written to disk and
        // read back by the next launch, as a different settings service.
        let settings = tempfile::tempdir().unwrap();
        let file = settings.path().join("settings.json");
        crate::service::settings::Service::new(file.clone())
            .save_transfers(&[crate::dto::SavedTransfer {
                phone: Some("ABC123".into()),
                job: crate::dto::SavedJob::Download {
                    request: s.request("/", &["video.mp4"]),
                    mode: crate::dto::Paste::Copy,
                    answer: crate::dto::SavedAnswer::NothingTaken,
                },
            }])
            .unwrap();
        let back = crate::service::settings::Service::new(file).load_transfers();
        let [
            crate::dto::SavedTransfer {
                job: crate::dto::SavedJob::Download { request, .. },
                ..
            },
        ] = back.as_slice()
        else {
            panic!("{back:?}");
        };

        let summary = s
            .service
            .download(request, &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 1);
        assert_eq!(summary.bytes, VIDEO as u64 - kept, "only the rest");
        assert_eq!(
            fs::read(s.mac.path().join("video.mp4")).unwrap(),
            video_bytes()
        );
    }

    #[tokio::test]
    async fn the_next_attempt_continues_from_the_part_and_copies_the_rest() {
        let s = setup().await;
        put_video(&s).await;
        cancel_the_video(&s).await;
        let kept = fs::metadata(s.mac.path().join(PART)).unwrap().len();
        let log = Log::default();

        let summary = s
            .service
            .download(&s.request("/", &["video.mp4"]), &Cancel::new(), |p| {
                log.record(p)
            })
            .await
            .unwrap();

        assert_eq!(s.mac_names(), ["video.mp4"], "the part file is gone");
        assert_eq!(
            fs::read(s.mac.path().join("video.mp4")).unwrap(),
            video_bytes(),
            "every byte in the right place"
        );
        assert_eq!(summary.resumed, 1);
        assert_eq!(summary.files, 1);
        assert_eq!(
            summary.bytes,
            VIDEO as u64 - kept,
            "only the rest was fetched"
        );
        let reports = log.take();
        assert_eq!(
            reports[0].bytes_total,
            VIDEO as u64 - kept,
            "totals count what is left"
        );
        assert_eq!(
            reports[0].file_bytes_done, kept,
            "the file bar starts where it left off"
        );
        assert_eq!(reports.last().unwrap().bytes_done, VIDEO as u64 - kept);
    }

    #[tokio::test]
    async fn a_download_cut_off_by_a_phone_reset_carries_on_after_a_reconnect_from_its_part() {
        let s = setup().await;
        put_video(&s).await;
        let serial = s.service.device.connect().await.unwrap().serial_number;
        let armed = Mutex::new(false);

        // A window has arrived when the phone resets (a cable, a lock).
        let cut = s
            .service
            .download(&s.request("/", &["video.mp4"]), &Cancel::new(), |p| {
                if p.file_bytes_done > 0
                    && p.file_bytes_done < p.file_bytes_total
                    && !std::mem::replace(&mut *armed.lock().unwrap(), true)
                {
                    mtp_rs::force_operation_wedge(&serial);
                }
            })
            .await;
        assert_eq!(cut, Err(Error::Disconnected));
        let kept = fs::metadata(s.mac.path().join(PART)).unwrap().len();
        assert!(kept > 0 && kept < VIDEO as u64, "what arrived is kept");

        // The same phone opens again, and the transfer carries on.
        s.service.device.connect().await.unwrap();
        let summary = s
            .service
            .download(&s.request("/", &["video.mp4"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 1);
        assert_eq!(summary.bytes, VIDEO as u64 - kept, "only the rest was read");
        assert_eq!(
            fs::read(s.mac.path().join("video.mp4")).unwrap(),
            video_bytes()
        );
        assert_eq!(s.mac_names(), ["video.mp4"]);
    }

    #[tokio::test]
    async fn a_file_changed_on_the_phone_is_not_continued() {
        let s = setup().await;
        put_video(&s).await;
        cancel_the_video(&s).await;
        // The phone's copy is edited: a new date.
        set_date(
            &s.phone.path().join("video.mp4"),
            phone_date(&s, "video.mp4") + Duration::from_secs(3_600),
        );
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        let summary = s
            .service
            .download(&s.request("/", &["video.mp4"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 0);
        assert_eq!(
            fs::read(s.mac.path().join("video.mp4")).unwrap(),
            video_bytes()
        );
        assert_eq!(summary.bytes, VIDEO as u64, "all of it, from the start");
    }

    #[tokio::test]
    async fn a_part_that_is_not_shorter_than_the_file_is_not_continued() {
        let s = setup().await;
        let part = s.mac.path().join(".notes.txt.open-mtp-client-part");
        fs::write(&part, b"hello").unwrap();
        set_date(&part, phone_date(&s, "notes.txt"));

        let summary = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 0);
        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn an_empty_part_is_not_continued() {
        let s = setup().await;
        let part = s.mac.path().join(".notes.txt.open-mtp-client-part");
        fs::write(&part, b"").unwrap();
        set_date(&part, phone_date(&s, "notes.txt"));

        let summary = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 0);
    }

    #[tokio::test]
    async fn a_phone_that_cannot_read_from_an_offset_never_resumes() {
        let s = setup_with(|phone| phone.partial_reads = false).await;
        let part = s.mac.path().join(".notes.txt.open-mtp-client-part");
        fs::write(&part, b"he").unwrap();
        set_date(&part, phone_date(&s, "notes.txt"));

        let summary = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 0);
        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn a_part_file_that_is_a_link_is_never_continued() {
        let s = setup().await;
        let victim = s.mac.path().join("victim.txt");
        fs::write(&victim, b"he").unwrap();
        set_date(&victim, phone_date(&s, "notes.txt"));
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("target");
        fs::write(&target, b"he").unwrap();
        set_date(&target, phone_date(&s, "notes.txt"));
        std::os::unix::fs::symlink(
            &target,
            s.mac.path().join(".notes.txt.open-mtp-client-part"),
        )
        .unwrap();

        let summary = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.resumed, 0);
        assert_eq!(
            fs::read(&target).unwrap(),
            b"he",
            "what the link points at is untouched"
        );
        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn a_part_swapped_for_another_before_the_copy_is_not_appended_to() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(".a.part");
        fs::write(&part, b"12345").unwrap();

        // The plan saw 3 bytes; now it is 5: not what was planned.
        assert!(open_resumable(&part, 3).await.is_none());
        assert!(open_resumable(&part, 5).await.is_some());
        assert!(
            open_resumable(&dir.path().join("missing"), 0)
                .await
                .is_none()
        );

        let target = dir.path().join("real");
        fs::write(&target, b"12345").unwrap();
        let link = dir.path().join(".link.part");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            open_resumable(&link, 5).await.is_none(),
            "a link is not a part file"
        );
        assert_eq!(fs::read(&target).unwrap(), b"12345");
    }

    #[tokio::test]
    async fn a_stale_part_found_at_the_copy_starts_the_file_over() {
        // The plan resumed at 3 bytes, but by the time of the copy the part
        // is 5 bytes: the copy starts over and the totals take the extra in.
        let s = setup().await;
        let part = s.mac.path().join(".notes.txt.open-mtp-client-part");
        fs::write(&part, b"hello").unwrap();
        let log = Log::default();
        let copy = Copy {
            storage: &s
                .service
                .device
                .session()
                .await
                .unwrap()
                .storage(mtp_rs::StorageId(s.storage_id))
                .await
                .unwrap(),
            handle: handle_of(&s, "notes.txt").await,
            partial_reads: true,
            cancel: &Cancel::new(),
            progress: &|p| log.record(p),
            started: Instant::now(),
            stall_limit: STALL_LIMIT,
            heard: s.service.device.activity(),
            moving: true,
            relayed: false,
        };
        let mut state = TransferProgress {
            bytes_total: 2,
            file_bytes_total: 5,
            ..TransferProgress::default()
        };
        let how = How::Resume {
            part: part.clone(),
            offset: 3,
        };

        let continued = copy
            .to(&s.mac.path().join("notes.txt"), &how, None, &mut state)
            .await
            .unwrap();

        assert!(!continued.resumed);
        assert_eq!(
            state.bytes_total, 5,
            "the offset it didn't use is back in the total"
        );
        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
    }

    async fn handle_of(s: &Setup, name: &str) -> ObjectHandle {
        let session = s.service.device.session().await.unwrap();
        let storage = session
            .storage(mtp_rs::StorageId(s.storage_id))
            .await
            .unwrap();
        let root = storage.list_objects(None).await.unwrap();
        root.iter().find(|o| o.filename == name).unwrap().handle
    }

    async fn settled(
        failure: Failure,
        copied: &[u8],
        when: Option<SystemTime>,
        resumable: bool,
    ) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(".x.part");
        fs::write(&part, copied).unwrap();

        settle_part(&part, &failure, when, resumable).await;

        part.exists()
    }

    #[tokio::test]
    async fn a_part_is_kept_when_the_phone_stopped_or_the_user_cancelled() {
        let when = Some(UNIX_EPOCH + Duration::from_secs(1_791_376_200));
        assert!(settled(Failure::Cancelled, b"abc", when, true).await);
        assert!(settled(Failure::Device(mtp_rs::Error::Timeout), b"abc", when, true).await);
    }

    #[tokio::test]
    async fn a_kept_part_is_stamped_with_the_phones_date() {
        let when = UNIX_EPOCH + Duration::from_secs(1_791_376_200);
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(".x.part");
        fs::write(&part, b"abc").unwrap();

        settle_part(&part, &Failure::Cancelled, Some(when), true).await;

        assert_eq!(fs::metadata(&part).unwrap().modified().unwrap(), when);
    }

    #[tokio::test]
    async fn a_part_is_removed_when_there_is_nothing_to_continue_from() {
        let when = Some(UNIX_EPOCH + Duration::from_secs(1_791_376_200));
        let mac_error = Failure::Mac(std::io::Error::other("disk full"), PathBuf::from("/x"));
        assert!(
            !settled(mac_error, b"abc", when, true).await,
            "a Mac-side failure would repeat"
        );
        assert!(
            !settled(Failure::Cancelled, b"", when, true).await,
            "nothing in it"
        );
        assert!(
            !settled(Failure::Cancelled, b"abc", None, true).await,
            "no date to stamp"
        );
        assert!(
            !settled(Failure::Cancelled, b"abc", when, false).await,
            "the phone can't continue"
        );
        assert!(!settled(Failure::TooLarge, b"abc", when, true).await);
    }

    #[tokio::test]
    async fn a_file_already_there_is_not_copied_again() {
        let s = setup().await;
        let first = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();
        assert_eq!((first.files, first.already_there), (1, 0));

        let again = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(s.mac_names(), ["notes.txt"], "no `notes (1).txt`");
        assert_eq!((again.files, again.bytes, again.already_there), (0, 0, 1));
        assert_eq!(again.renamed, 0);
        assert_eq!(again.first_item.as_deref(), Some("notes.txt"));
    }

    #[tokio::test]
    async fn retrying_a_batch_copies_only_what_is_missing() {
        let s = setup().await;
        let cancel = Cancel::new();
        let trigger = cancel.clone();
        let request = s.request("/DCIM/Camera", &["a.jpg", "b.jpg"]);
        // The first attempt stops after a.jpg.
        let result = s
            .service
            .download(&request, &cancel, |p| {
                if p.file_bytes_total > 0 && p.file_bytes_done == p.file_bytes_total {
                    trigger.cancel();
                }
            })
            .await;
        assert_eq!(result, Err(Error::Cancelled));
        let log = Log::default();

        let summary = s
            .service
            .download(&request, &Cancel::new(), |p| log.record(p))
            .await
            .unwrap();

        assert_eq!(s.mac_names(), ["a.jpg", "b.jpg"], "no duplicates");
        assert_eq!((summary.files, summary.already_there), (1, 1));
        assert_eq!(summary.bytes, 2, "only b.jpg's bytes");
        let reports = log.take();
        assert_eq!(reports[0].files_total, 2);
        assert_eq!(
            reports[0].bytes_total, 2,
            "a.jpg isn't in what is left to copy"
        );
        let last = reports.last().unwrap();
        assert_eq!((last.files_done, last.files_total), (2, 2));
    }

    /// What Pause and Resume do: the same request again after a cancel.
    #[tokio::test]
    async fn a_download_stopped_part_way_through_a_file_carries_on_from_its_part() {
        let movie: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let s = setup_with(|phone| {
            fs::write(phone.backing_dir.join("DCIM/Camera/c.mp4"), &movie).unwrap();
        })
        .await;
        let request = s.request("/DCIM/Camera", &["a.jpg", "c.mp4"]);
        let cancel = Cancel::new();
        let trigger = cancel.clone();
        // Stopped after c.mp4's first window.
        let stopped = s
            .service
            .download(&request, &cancel, |p| {
                if p.file_bytes_total == movie.len() as u64 && p.file_bytes_done > 0 {
                    trigger.cancel();
                }
            })
            .await;
        assert_eq!(stopped, Err(Error::Cancelled));
        let part = s.mac.path().join(".c.mp4.open-mtp-client-part");
        let kept = fs::metadata(&part).unwrap().len();
        assert_eq!(kept, u64::from(WINDOW), "one window kept");
        let log = Log::default();

        let summary = s
            .service
            .download(&request, &Cancel::new(), |p| log.record(p))
            .await
            .unwrap();

        assert_eq!(fs::read(s.mac.path().join("c.mp4")).unwrap(), movie);
        assert!(!part.exists());
        assert_eq!(summary.already_there, 1, "a.jpg isn't copied again");
        assert_eq!(
            summary.bytes,
            movie.len() as u64 - kept,
            "only what the part didn't hold came from the phone"
        );
        assert_eq!(log.take()[0].bytes_total, movie.len() as u64 - kept);
    }

    #[tokio::test]
    async fn retrying_a_whole_folder_copies_nothing_twice() {
        let s = setup().await;
        s.service
            .download(&s.request("/", &["DCIM"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        let again = s
            .service
            .download(&s.request("/", &["DCIM"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(s.mac_names(), ["DCIM"], "no `DCIM (1)`");
        assert_eq!((again.files, again.already_there), (0, 2));
        assert_eq!(again.renamed, 0);
    }

    #[tokio::test]
    async fn new_files_in_a_folder_are_added_to_the_copy_made_before() {
        let s = setup().await;
        s.service
            .download(&s.request("/DCIM", &["Camera"]), &Cancel::new(), |_| {})
            .await
            .unwrap();
        fs::write(s.phone.path().join("DCIM/Camera/c.jpg"), b"new").unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        let summary = s
            .service
            .download(&s.request("/DCIM", &["Camera"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert!(s.mac.path().join("Camera/c.jpg").exists());
        assert_eq!((summary.files, summary.already_there), (1, 2));
    }

    #[tokio::test]
    async fn a_file_with_the_same_size_but_another_date_is_not_the_same_file() {
        let s = setup().await;
        let mine = s.mac.path().join("notes.txt");
        fs::write(&mine, b"HELLO").unwrap();
        set_date(
            &mine,
            phone_date(&s, "notes.txt") + Duration::from_secs(3_600),
        );

        let summary = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(fs::read(&mine).unwrap(), b"HELLO");
        assert_eq!(
            fs::read(s.mac.path().join("notes (1).txt")).unwrap(),
            b"hello"
        );
        assert_eq!((summary.renamed, summary.already_there), (1, 0));
    }

    #[tokio::test]
    async fn a_copy_dated_up_to_two_seconds_off_still_counts_as_the_same() {
        let s = setup().await;
        let copy = s.mac.path().join("notes.txt");
        fs::write(&copy, b"hello").unwrap();
        set_date(&copy, phone_date(&s, "notes.txt") + Duration::from_secs(2));

        let near = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(near.already_there, 1, "FAT keeps even seconds only");

        set_date(&copy, phone_date(&s, "notes.txt") + Duration::from_secs(10));
        let far = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();
        assert_eq!((far.already_there, far.renamed), (0, 1));
    }

    #[tokio::test]
    async fn a_numbered_copy_made_before_counts_so_nothing_is_copied_a_third_time() {
        let s = setup().await;
        // notes.txt is someone else's; the phone's was kept as `notes (1).txt`.
        fs::write(s.mac.path().join("notes.txt"), b"mine").unwrap();
        s.service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        let again = s
            .service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(
            s.mac_names(),
            ["notes (1).txt", "notes.txt"],
            "no `notes (2).txt`"
        );
        assert_eq!((again.files, again.already_there, again.renamed), (0, 1, 0));
        assert_eq!(again.first_item.as_deref(), Some("notes (1).txt"));
    }

    #[tokio::test]
    async fn a_file_without_a_date_cannot_be_called_the_same() {
        let cases: [(Option<DateTime>, bool); 1] = [(None, false)];
        for (modified, same) in cases {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.jpg"), b"x").unwrap();

            let (target, how) = resolve_file(
                &dir.path().join("a.jpg"),
                1,
                modified,
                true,
                &HashSet::new(),
            )
            .await;

            assert_eq!(how == How::AlreadyThere, same);
            assert_eq!(
                target,
                dir.path().join("a (1).jpg"),
                "kept beside it instead"
            );
        }
    }

    #[tokio::test]
    async fn merging_a_folder_never_writes_through_a_link() {
        let s = setup().await;
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), s.mac.path().join("DCIM")).unwrap();

        let summary = s
            .service
            .download(&s.request("/", &["DCIM"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert!(
            fs::read_dir(elsewhere.path()).unwrap().next().is_none(),
            "nothing written through the link"
        );
        assert!(s.mac.path().join("DCIM (1)/Camera/a.jpg").exists());
        assert_eq!(summary.renamed, 1);
    }

    #[tokio::test]
    async fn a_file_in_the_way_of_a_folder_is_kept_and_the_folder_numbered() {
        let s = setup().await;
        fs::write(s.mac.path().join("DCIM"), b"not a folder").unwrap();

        s.service
            .download(&s.request("/", &["DCIM"]), &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(
            fs::read(s.mac.path().join("DCIM")).unwrap(),
            b"not a folder"
        );
        assert!(s.mac.path().join("DCIM (1)/Camera/a.jpg").exists());
    }

    #[tokio::test]
    async fn progress_names_the_final_names_of_what_it_copies() {
        let s = setup().await;
        fs::write(s.mac.path().join("notes.txt"), b"mine").unwrap();
        let log = Log::default();

        s.service
            .download(&s.request("/", &["notes.txt"]), &Cancel::new(), |p| {
                log.record(p)
            })
            .await
            .unwrap();

        assert!(log.take().iter().any(|p| p.current_file == "notes (1).txt"));
    }

    #[tokio::test]
    async fn planning_a_big_folder_stays_quick() {
        // A camera folder: every file is looked up at the destination to see
        // whether it is already there, so this must stay cheap per file.
        let mac = tempfile::tempdir().unwrap();
        let photos: Vec<Node> = (0..5_000)
            .map(|i| Node {
                handle: ObjectHandle(10 + i as u64),
                name: format!("IMG_{i:06}.jpg"),
                is_folder: false,
                size: 1_000,
                modified: Some(DateTime {
                    year: 2026,
                    month: 10,
                    day: 7,
                    hour: 13,
                    minute: 50,
                    second: 0,
                    offset: None,
                }),
            })
            .collect();
        let device = tree(&[(1, photos)]);

        let started = Instant::now();
        let plan = plan(&device, &[node(1, "Camera", true)], mac.path(), true)
            .await
            .unwrap();

        assert_eq!(plan.steps.len(), 5_001);
        // About 0.1 s in a debug build; the bound only catches a change that
        // makes it cost per-file work many times over.
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_matching_numbered_part_is_found_behind_one_that_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let when = UNIX_EPOCH + Duration::from_secs(1_791_376_200);
        let target = dir.path().join("video.mp4");
        // Part 0 is somebody else's leftover (wrong date); part 1 is ours.
        let stale = dir.path().join(".video.mp4.open-mtp-client-part");
        fs::write(&stale, b"junk").unwrap();
        set_date(&stale, when + Duration::from_secs(3_600));
        let ours = dir.path().join(".video.mp4.1.open-mtp-client-part");
        fs::write(&ours, b"abcdef").unwrap();
        set_date(&ours, when);

        let found = resumable_part(&target, 100, when).await;

        assert_eq!(found, Some((ours, 6)));
    }

    #[tokio::test]
    async fn no_part_means_a_fresh_copy_for_the_price_of_one_look() {
        let dir = tempfile::tempdir().unwrap();
        let when = UNIX_EPOCH + Duration::from_secs(1_791_376_200);

        assert_eq!(
            resumable_part(&dir.path().join("a.jpg"), 100, when).await,
            None
        );
    }

    #[test]
    fn a_file_may_send_up_to_its_reported_size_and_not_a_byte_more() {
        assert!(check_within_size(0, 100, 100).is_ok());
        assert!(check_within_size(60, 40, 100).is_ok(), "exactly to the end");
        assert!(check_within_size(100, 0, 100).is_ok());
        assert!(check_within_size(60, 41, 100).is_err());
        assert!(check_within_size(100, 1, 100).is_err());
        assert!(
            check_within_size(0, 1, 0).is_err(),
            "a file reported empty sends nothing"
        );
    }

    #[test]
    fn sending_too_much_is_a_device_error() {
        let Err(Failure::Device(err)) = check_within_size(0, 2, 1) else {
            panic!("expected a device error");
        };
        assert!(err.to_string().contains("more of a file"));
    }

    #[test]
    fn a_huge_count_cannot_wrap_round_the_limit() {
        assert!(check_within_size(u64::MAX - 1, 10, 100).is_err());
    }

    #[test]
    fn four_gigabytes_or_more_is_no_size_at_all() {
        let marker = u64::from(u32::MAX);
        assert_eq!(size_limit(marker), u64::MAX);
        assert_eq!(size_limit(marker - 1), marker - 1);
        assert_eq!(size_limit(0), 0);
        // So a real 5 GB file behind the marker is accepted in full...
        assert!(check_within_size(5_000_000_000, 2_000_000, size_limit(marker)).is_ok());
        // ...and a cancel is honoured however much has arrived.
        assert!(cancel_stops(true, 5_000_000_000, size_limit(marker)));
    }

    #[test]
    fn a_cancel_stops_everything_except_a_file_that_has_fully_arrived() {
        assert!(!cancel_stops(false, 10, 100), "no cancel, no stop");
        assert!(cancel_stops(true, 10, 100), "part way");
        assert!(cancel_stops(true, 0, 100), "not started");
        assert!(
            !cancel_stops(true, 100, 100),
            "complete: placed, then the batch stops"
        );
        // The phone can't make a file look complete by reporting a small size:
        // sending past it is an error before the cancel is even looked at.
        assert!(check_within_size(100, 1, 100).is_err());
    }

    /// Makes the phone's video bigger than the size it reported, once, at the
    /// first progress report (after planning, before any byte is read).
    fn grow_the_video_once(s: &Setup) -> impl Fn() + '_ {
        let video = s.phone.path().join("video.mp4");
        let grown = Mutex::new(false);
        move || {
            if !std::mem::replace(&mut *grown.lock().unwrap(), true) {
                let mut more = fs::read(&video).unwrap();
                more.extend(vec![9u8; 3 * 1024 * 1024]);
                fs::write(&video, more).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn a_file_that_sends_more_than_its_reported_size_is_a_device_error() {
        let s = setup().await;
        put_video(&s).await;
        let grow = grow_the_video_once(&s);

        let result = s
            .service
            .download(&s.request("/", &["video.mp4"]), &Cancel::new(), |_| grow())
            .await;

        let Err(Error::General(detail)) = result else {
            panic!("expected a device error, got {result:?}");
        };
        assert!(detail.contains("more of a file"), "{detail}");
        assert!(
            !s.mac.path().join("video.mp4").exists(),
            "nothing under the real name"
        );
        let kept = fs::metadata(s.mac.path().join(PART)).map_or(0, |m| m.len());
        assert!(
            kept <= VIDEO as u64,
            "{kept} bytes written for a file reported as {VIDEO}"
        );
    }

    #[tokio::test]
    async fn a_cancel_still_stops_a_file_that_is_growing() {
        let s = setup().await;
        put_video(&s).await;
        let grow = grow_the_video_once(&s);
        let cancel = Cancel::new();
        let trigger = cancel.clone();

        let result = s
            .service
            .download(&s.request("/", &["video.mp4"]), &cancel, |p| {
                grow();
                if p.file_bytes_done > 0 {
                    trigger.cancel();
                }
            })
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        assert!(!s.mac.path().join("video.mp4").exists());
    }

    /// Makes `folder` (inside the Mac destination) refuse new files.
    fn make_read_only(folder: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
    }

    fn make_writable(folder: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A Mac destination where `DCIM` exists with a read-only `Camera` in it.
    fn with_a_locked_camera_folder(s: &Setup) -> PathBuf {
        let camera = s.mac.path().join("DCIM/Camera");
        fs::create_dir_all(&camera).unwrap();
        make_read_only(&camera);
        camera
    }

    #[tokio::test]
    async fn one_file_that_cannot_be_written_does_not_stop_the_rest() {
        let s = setup().await;
        let camera = with_a_locked_camera_folder(&s);

        let summary = s
            .service
            .download(
                &s.request("/", &["DCIM", "notes.txt"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();
        make_writable(&camera);

        assert_eq!(fs::read(s.mac.path().join("notes.txt")).unwrap(), b"hello");
        assert_eq!((summary.files, summary.failures.len()), (1, 2));
        // In the phone's own order, whatever that is.
        let mut labels: Vec<_> = summary.failures.iter().map(|f| f.label.as_str()).collect();
        labels.sort();
        assert_eq!(labels, ["DCIM/Camera/a.jpg", "DCIM/Camera/b.jpg"]);
        assert!(matches!(
            summary.failures[0].error,
            Error::FolderAccessDenied(_)
        ));
        assert!(!camera.join("a.jpg").exists());
    }

    #[tokio::test]
    async fn progress_stays_honest_when_a_file_fails() {
        let s = setup().await;
        let camera = with_a_locked_camera_folder(&s);
        let log = Log::default();

        s.service
            .download(
                &s.request("/", &["DCIM", "notes.txt"]),
                &Cancel::new(),
                |p| log.record(p),
            )
            .await
            .unwrap();
        make_writable(&camera);

        let reports = log.take();
        let last = reports.last().unwrap();
        assert_eq!(
            (last.files_done, last.files_total),
            (3, 3),
            "every file was dealt with"
        );
        assert_eq!(last.bytes_done, 5, "only notes.txt's bytes moved");
        assert_eq!(
            last.bytes_total, 5,
            "what could not be sent is no longer owed"
        );
    }

    #[tokio::test]
    async fn a_folder_that_cannot_be_made_skips_what_was_to_go_in_it() {
        let s = setup().await;
        let dcim = s.mac.path().join("DCIM");
        fs::create_dir(&dcim).unwrap();
        make_read_only(&dcim);
        let log = Log::default();

        let summary = s
            .service
            .download(
                &s.request("/", &["DCIM", "notes.txt"]),
                &Cancel::new(),
                |p| log.record(p),
            )
            .await
            .unwrap();
        make_writable(&dcim);

        let labels: Vec<_> = summary.failures.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            ["DCIM/Camera"],
            "one failure for the folder, none for its files"
        );
        assert_eq!(summary.files, 1, "notes.txt");
        let last = log.take().pop().unwrap();
        assert_eq!((last.files_done, last.files_total), (3, 3));
    }

    #[tokio::test]
    async fn retrying_after_a_failure_copies_only_what_failed() {
        let s = setup().await;
        let camera = with_a_locked_camera_folder(&s);
        let request = s.request("/", &["DCIM", "notes.txt"]);
        let first = s
            .service
            .download(&request, &Cancel::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(first.failures.len(), 2);
        // The folder is fixed; try again.
        make_writable(&camera);

        let second = s
            .service
            .download(&request, &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert!(second.failures.is_empty());
        assert_eq!(
            (second.files, second.already_there),
            (2, 1),
            "the two that failed are copied; notes.txt is already there"
        );
        assert!(camera.join("a.jpg").exists() && camera.join("b.jpg").exists());
        assert_eq!(s.mac_names(), ["DCIM", "notes.txt"], "no duplicates");
    }

    #[tokio::test]
    async fn a_phone_that_resets_mid_batch_stops_it() {
        let s = setup().await;
        let serial = s.service.device.connect().await.unwrap().serial_number;
        let armed = Mutex::new(false);

        let result = s
            .service
            .download(
                &s.request("/DCIM/Camera", &["a.jpg", "b.jpg"]),
                &Cancel::new(),
                |p| {
                    // As the first file starts, the phone wedges: its next
                    // operation (the read) comes back as a reset.
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
            !s.mac.path().join("b.jpg").exists(),
            "the batch did not go on"
        );
    }

    #[test]
    fn a_failure_about_one_object_lets_the_batch_go_on() {
        use mtp_rs::Error as Phone;
        for err in [
            Phone::NotFound,
            Phone::StaleHandle,
            Phone::AccessDenied,
            Phone::Unsupported,
            Phone::Busy,
            Phone::InvalidData {
                message: "x".into(),
            },
            Phone::Other {
                detail: "GeneralError".into(),
            },
        ] {
            assert!(!Failure::Device(err).stops_the_batch());
        }
        let io = std::io::Error::from(ErrorKind::PermissionDenied);
        assert!(!Failure::Mac(io, PathBuf::from("/x")).stops_the_batch());
    }

    #[test]
    fn a_failure_about_the_connection_the_disk_or_the_user_stops_the_batch() {
        use mtp_rs::Error as Phone;
        for err in [
            Phone::Disconnected,
            Phone::DeviceReset,
            Phone::Timeout,
            Phone::NoDevice,
            Phone::ExclusiveAccess,
            Phone::PermissionDenied,
            Phone::StorageFull,
            Phone::Cancelled,
            Phone::Io {
                message: "x".into(),
            },
        ] {
            assert!(Failure::Device(err).stops_the_batch());
        }
        let full = std::io::Error::from(ErrorKind::StorageFull);
        assert!(Failure::Mac(full, PathBuf::from("/x")).stops_the_batch());
        assert!(Failure::Cancelled.stops_the_batch());
        assert!(Failure::UnsafeName("../x".into()).stops_the_batch());
        assert!(Failure::TooLarge.stops_the_batch());
    }

    #[test]
    fn nothing_copied_for_one_reason_is_the_batchs_own_error() {
        let failed = |label: &str, error: Error| FileFailure {
            label: label.into(),
            error,
        };
        let denied = || Error::FolderAccessDenied("/x".into());

        let same = [failed("a", denied()), failed("b", denied())];
        assert_eq!(whole_batch_failure(0, 0, &same), Some(denied()));
        assert_eq!(
            whole_batch_failure(0, 0, &same[..1]),
            Some(denied()),
            "one file"
        );

        let mixed = [failed("a", denied()), failed("b", Error::Timeout)];
        assert_eq!(
            whole_batch_failure(0, 0, &mixed),
            None,
            "different reasons: a list"
        );
        assert_eq!(
            whole_batch_failure(1, 0, &same),
            None,
            "something was copied"
        );
        assert_eq!(
            whole_batch_failure(0, 1, &same),
            None,
            "something was already there"
        );
        assert_eq!(whole_batch_failure(0, 0, &[]), None);
    }

    #[tokio::test]
    async fn a_phone_that_goes_quiet_is_given_up_on() {
        let result = phone_answers(
            &device::Activity::default(),
            Duration::from_millis(50),
            std::future::pending::<u8>(),
        )
        .await;

        assert!(matches!(
            result,
            Err(Failure::Device(mtp_rs::Error::Timeout))
        ));
    }

    /// A request behind another transfer on the phone's one session: asking
    /// for the storage again, which waits until the session is free.
    async fn asked_behind(s: &Setup, limit: Duration) -> Result<(), Failure> {
        let session = s.service.device.session().await.unwrap();
        phone_answers(
            &s.service.device.activity(),
            limit,
            session.storage(mtp_rs::StorageId(s.storage_id)),
        )
        .await
        .map(|_| ())
    }

    #[tokio::test]
    async fn a_request_queued_behind_a_busy_transfer_is_waited_for_not_given_up_on() {
        let s = setup().await;
        let limit = Duration::from_millis(150);
        // Twelve chunks a third of the limit apart: four limits in all, but
        // never a whole limit without word from the phone.
        let _busy =
            crate::service::testing::busy(&s.service.device, s.storage_id, 12, limit / 3).await;
        let started = std::time::Instant::now();

        let result = asked_behind(&s, limit).await;

        assert!(result.is_ok(), "{result:?}");
        assert!(
            started.elapsed() > limit,
            "it really waited behind the transfer"
        );
    }

    #[tokio::test]
    async fn a_request_queued_behind_a_silent_phone_ends_as_a_timeout() {
        let s = setup().await;
        let limit = Duration::from_millis(150);
        let _silence = crate::service::testing::silence(&s.service.device, s.storage_id).await;

        let result = asked_behind(&s, limit).await;

        assert!(matches!(
            result,
            Err(Failure::Device(mtp_rs::Error::Timeout))
        ));
    }

    #[tokio::test]
    async fn a_phone_that_answers_in_time_is_not_interrupted() {
        let result = phone_answers(
            &device::Activity::default(),
            Duration::from_secs(5),
            async { 7 },
        )
        .await;

        assert!(matches!(result, Ok(7)));
    }

    #[tokio::test]
    async fn the_stall_limit_is_generous_by_default() {
        let s = setup().await;

        assert_eq!(s.service.stall_limit, STALL_LIMIT);
        assert!(
            STALL_LIMIT >= Duration::from_secs(60),
            "a slow window must not look stalled"
        );
        assert_eq!(
            Service::new(
                Arc::clone(&s.service.device),
                Arc::clone(&s.service.explorer)
            )
            .with_stall_limit(Duration::from_secs(1))
            .stall_limit,
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn a_cancel_during_the_last_file_is_a_cancel_not_a_listed_failure() {
        let s = setup().await;
        put_video(&s).await;
        let cancel = Cancel::new();
        let trigger = cancel.clone();

        // notes.txt finishes, then the video is cancelled part way: the end of
        // the batch must be `Cancelled`, not a success with a failure in it.
        let result = s
            .service
            .download(&s.request("/", &["notes.txt", "video.mp4"]), &cancel, |p| {
                if p.file_bytes_total > 5_000_000
                    && p.file_bytes_done > 0
                    && p.file_bytes_done < p.file_bytes_total
                {
                    trigger.cancel();
                }
            })
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        assert!(
            s.mac.path().join("notes.txt").exists(),
            "the finished file is kept"
        );
        assert!(
            s.mac.path().join(PART).exists(),
            "the video can be continued"
        );
    }

    /// The pane showing a phone folder in full, which is what lets the next
    /// action on it skip reading it again.
    async fn pane_shows(s: &Setup, path: &str) -> Vec<String> {
        let mut listing = s
            .service
            .explorer
            .open_listing(s.storage_id, path, false, Cancel::new(), false)
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
    async fn downloading_from_a_remembered_folder_asks_about_the_chosen_files_only() {
        let s = setup().await;
        assert_eq!(pane_shows(&s, "/DCIM/Camera").await, ["a.jpg", "b.jpg"]);
        let reads = s.service.explorer.full_reads();

        let summary = s
            .service
            .download(
                &s.request("/DCIM/Camera", &["a.jpg"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(summary.files, 1);
        assert_eq!(fs::read(s.mac.path().join("a.jpg")).unwrap(), b"aaaa");
        assert_eq!(
            s.service.explorer.full_reads(),
            reads,
            "the folder was not read again to find one file"
        );
    }

    #[tokio::test]
    async fn a_download_takes_the_size_and_date_the_phone_gives_now_not_the_remembered_ones() {
        let s = setup().await;
        pane_shows(&s, "/DCIM/Camera").await;
        // The file grows on the phone after the pane listed it (same count).
        let bigger = vec![b'z'; 3000];
        fs::write(s.phone.path().join("DCIM/Camera/a.jpg"), &bigger).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        s.service
            .download(
                &s.request("/DCIM/Camera", &["a.jpg"]),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(
            fs::read(s.mac.path().join("a.jpg")).unwrap(),
            bigger,
            "all of the file as it is now, not the 4 bytes that were remembered"
        );
    }

    #[tokio::test]
    async fn downloading_a_file_that_is_gone_is_a_clear_error_not_a_stale_copy() {
        let s = setup().await;
        pane_shows(&s, "/DCIM/Camera").await;
        fs::remove_file(s.phone.path().join("DCIM/Camera/a.jpg")).unwrap();
        fs::write(s.phone.path().join("DCIM/Camera/c.jpg"), b"c").unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        let result = s
            .service
            .download(
                &s.request("/DCIM/Camera", &["a.jpg"]),
                &Cancel::new(),
                |_| {},
            )
            .await;

        assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
        assert!(s.mac_names().is_empty());
    }

    // ---- what a move needs to know ----

    #[tokio::test]
    async fn the_plan_remembers_where_every_original_is() {
        let mac = tempfile::tempdir().unwrap();
        // 1 = DCIM { 2 = a.jpg, 3 = Camera { 4 = b.jpg } }, 5 = notes.txt
        let device = tree(&[
            (1, vec![node(2, "a.jpg", false), node(3, "Camera", true)]),
            (3, vec![node(4, "b.jpg", false)]),
        ]);

        let plan = plan(
            &device,
            &[node(1, "DCIM", true), node(5, "notes.txt", false)],
            mac.path(),
            true,
        )
        .await
        .unwrap();

        let seen: Vec<_> = plan
            .origins
            .iter()
            .map(|o| {
                (
                    o.name.as_str(),
                    o.parent.as_str(),
                    o.top,
                    o.is_top,
                    o.is_folder,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("DCIM", "", 0, true, true),
                ("a.jpg", "DCIM", 0, false, false),
                ("Camera", "DCIM", 0, false, true),
                ("b.jpg", "DCIM/Camera", 0, false, false),
                ("notes.txt", "", 1, true, false),
            ]
        );
        assert_eq!(plan.origins.len(), plan.steps.len(), "one origin per step");
        assert_eq!(plan.items, ["DCIM", "notes.txt"]);
        assert!(!plan.unwalked);
    }

    #[tokio::test]
    async fn an_original_keeps_the_phones_name_when_the_mac_copy_is_numbered() {
        let mac = tempfile::tempdir().unwrap();
        fs::write(mac.path().join("a.jpg"), b"different").unwrap();
        let device = tree(&[]);

        let plan = plan(&device, &[node(1, "a.jpg", false)], mac.path(), true)
            .await
            .unwrap();

        assert!(matches!(&plan.steps[0], Step::File { label, .. } if label == "a (1).jpg"));
        assert_eq!(
            plan.origins[0].name, "a.jpg",
            "the phone's name, to find it again"
        );
    }

    #[tokio::test]
    async fn a_folder_met_twice_is_noted_so_a_move_never_removes_folders() {
        let mac = tempfile::tempdir().unwrap();
        let device = tree(&[(1, vec![node(2, "a.jpg", false), node(1, "Loop", true)])]);

        let plan = plan(&device, &[node(1, "Loop", true)], mac.path(), true)
            .await
            .unwrap();

        assert!(plan.unwalked);
    }

    fn origin(name: &str, parent: &str, is_folder: bool) -> Origin {
        Origin {
            handle: ObjectHandle(1),
            parent: parent.into(),
            name: name.into(),
            size: 1,
            modified: None,
            is_folder,
            top: 0,
            is_top: parent.is_empty(),
        }
    }

    #[test]
    fn only_checked_files_and_clear_folders_are_removed() {
        let none = HashSet::new();
        let file = origin("a.jpg", "DCIM", false);
        let folder = origin("Camera", "DCIM", true);

        assert!(should_remove(&file, Outcome::Verified, false, &none));
        assert!(!should_remove(&file, Outcome::Unchecked, false, &none));
        assert!(!should_remove(&file, Outcome::NotCopied, false, &none));
        assert!(should_remove(&folder, Outcome::Verified, false, &none));
        assert!(!should_remove(&folder, Outcome::NotCopied, false, &none));
        assert!(
            !should_remove(&folder, Outcome::Unchecked, false, &none),
            "a folder is only ever created, never unchecked"
        );
    }

    #[test]
    fn a_folder_with_something_staying_inside_stays() {
        let folder = origin("Camera", "DCIM", true);
        let mut blocked = HashSet::new();
        block(&mut blocked, "DCIM/Camera");

        assert!(!should_remove(&folder, Outcome::Verified, false, &blocked));
        // The folders above it stay too; a sibling does not.
        assert!(!should_remove(
            &origin("DCIM", "", true),
            Outcome::Verified,
            false,
            &blocked
        ));
        assert!(should_remove(
            &origin("Other", "DCIM", true),
            Outcome::Verified,
            false,
            &blocked
        ));
    }

    #[test]
    fn a_partly_walked_tree_never_loses_a_folder_but_may_lose_checked_files() {
        let none = HashSet::new();

        assert!(!should_remove(
            &origin("DCIM", "", true),
            Outcome::Verified,
            true,
            &none
        ));
        assert!(should_remove(
            &origin("a.jpg", "DCIM", false),
            Outcome::Verified,
            true,
            &none
        ));
    }

    #[test]
    fn blocking_a_folder_blocks_every_folder_above_it() {
        let mut blocked = HashSet::new();

        block(&mut blocked, "A/B/C");
        block(&mut blocked, "");

        let mut got: Vec<_> = blocked.into_iter().collect();
        got.sort();
        assert_eq!(got, ["A", "A/B", "A/B/C"]);
    }

    #[test]
    fn phone_paths_join_without_doubled_slashes() {
        assert_eq!(absolute("/", ""), "/");
        assert_eq!(absolute("/", "DCIM"), "/DCIM");
        assert_eq!(absolute("/DCIM", "Camera/x"), "/DCIM/Camera/x");
        assert_eq!(absolute("/DCIM/", "Camera"), "/DCIM/Camera");
    }

    #[test]
    fn a_file_already_there_counts_as_checked_only_if_the_phone_gave_a_size() {
        assert_eq!(Outcome::of_existing(5), Outcome::Verified);
        assert_eq!(Outcome::of_existing(0), Outcome::Verified);
        assert_eq!(
            Outcome::of_existing(u64::from(u32::MAX)),
            Outcome::Unchecked
        );
    }

    #[test]
    fn a_size_the_phone_did_not_give_cannot_be_checked() {
        assert!(size_is_known(0));
        assert!(size_is_known(5_000_000_000));
        assert!(!size_is_known(u64::from(u32::MAX)));
    }

    #[tokio::test]
    async fn a_copy_is_verified_only_when_everything_arrived_at_the_size_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        fs::write(&file, b"12345").unwrap();

        assert!(verify_placed(&file, 5, 5).await);
        assert!(
            !verify_placed(&file, u64::from(u32::MAX), 5).await,
            "no size to check against"
        );
        assert!(
            !verify_placed(&file, 5, 4).await,
            "less arrived than reported"
        );
        assert!(
            !verify_placed(&file, 6, 6).await,
            "the file on disk is not that long"
        );
        assert!(!verify_placed(&dir.path().join("gone"), 5, 5).await);
        assert!(
            !verify_placed(dir.path(), 5, 5).await,
            "a folder is not a copy"
        );
    }

    #[test]
    fn what_a_phone_says_no_to_is_kept_and_what_it_cannot_answer_ends_the_removal() {
        assert!(matches!(
            refused(mtp_rs::Error::NotFound),
            Removed::Kept(Why::Changed)
        ));
        assert!(matches!(
            refused(mtp_rs::Error::StaleHandle),
            Removed::Kept(Why::Changed)
        ));
        assert!(matches!(
            refused(mtp_rs::Error::AccessDenied),
            Removed::Kept(Why::Refused(_))
        ));
        assert!(matches!(
            refused(mtp_rs::Error::Timeout),
            Removed::Stopped(Failure::Device(mtp_rs::Error::Timeout))
        ));
        assert!(matches!(
            refused(mtp_rs::Error::Disconnected),
            Removed::Stopped(_)
        ));
    }
}
