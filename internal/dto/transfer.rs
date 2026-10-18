use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::mirror::Mirrored;

use super::Error;

/// Which way a transfer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Phone → Mac.
    #[default]
    Download,
    /// Mac → phone.
    Upload,
    /// Inside the Mac: a copy or move from one Mac folder to another.
    Local,
}

/// What to do with an item whose name is already taken on the phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Leave the existing item alone and don't copy this one.
    Skip,
    /// Copy this one under a numbered name: `IMG_001 (1).jpg`.
    KeepBoth,
    /// Files replace the existing item; folders merge into it.
    Replace,
    /// What a retry of a move uses, after a first try that stopped part way:
    /// a folder merges into the one of the same name (the one the first try
    /// made), a file that is already there as it is (same size and date) is
    /// left alone, and a different file or a file where a folder is gets a
    /// numbered name beside it. Nothing on the phone is replaced.
    Merge,
    /// A file replaces the phone's only when it is newer (by more than the
    /// two seconds file systems round to); the rest stay as they are, and
    /// folders merge. Nothing older ever replaces anything; a file where a
    /// folder is, or the other way round, is left alone. For copies only: a
    /// move would put an original whose copy was not sent in the Trash.
    ReplaceOlder,
}

/// Copy Mac items (absolute paths, possibly from different folders, as a
/// Finder drop can be) into `destination`, a folder on a phone storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRequest {
    pub sources: Vec<String>,
    pub storage_id: u64,
    pub destination: String,
}

/// Copy `names` from `source` (a folder on a phone storage) into
/// `destination` (a folder on the Mac).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub storage_id: u64,
    pub source: String,
    pub names: Vec<String>,
    pub destination: String,
}

/// Where a running transfer is, reported after every chunk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferProgress {
    pub direction: Direction,
    /// Path of the file being copied, relative to the destination.
    pub current_file: String,
    pub files_done: usize,
    pub files_total: usize,
    pub file_bytes_done: u64,
    pub file_bytes_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub elapsed: Duration,
    /// Every byte of the current file has been sent or received and what is
    /// left is the phone or the disk settling it: the last moments that can
    /// look like a hang at 100%.
    pub finishing: bool,
    /// This is a move: the copy is followed by removing the originals.
    pub moving: bool,
    /// A copy or move inside the phone that goes through the Mac, because the
    /// phone can't copy or move by itself: `direction` says which half this is.
    pub through_mac: bool,
    /// The copy is done and a move is removing the originals from the phone;
    /// `files_done` and `files_total` then count the originals.
    pub removing: bool,
    /// With `removing`: the originals are Mac items going to the Trash (a move
    /// to the phone), so `files_done` and `files_total` count items, one Trash
    /// call each, not files.
    pub trashing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferSummary {
    pub direction: Direction,
    pub files: usize,
    pub bytes: u64,
    /// Top-level items saved under a new name because the name was taken.
    pub renamed: usize,
    /// Top-level items left alone because their name was taken.
    pub skipped: usize,
    /// Top-level items that replaced (or, for folders, merged into) one with the same name.
    pub replaced: usize,
    /// Top-level folders that were merged into the folder of the same name,
    /// which a retry of a move does instead of making a numbered copy.
    pub merged: usize,
    /// Files not copied because an identical one (same size and date) was
    /// already at the destination, e.g. when a failed batch is retried.
    pub already_there: usize,
    /// Files that continued from what an earlier attempt had already copied.
    pub resumed: usize,
    /// Files that could not be copied while the rest went on.
    pub failures: Vec<FileFailure>,
    pub destination: String,
    /// The first item copied, as it is named at the destination.
    pub first_item: Option<String>,
    /// What a move did with the originals once the copies were checked;
    /// `None` for a copy.
    pub removal: Option<Removal>,
    /// This was a copy or move inside the phone, done through the Mac.
    pub through_mac: bool,
    /// For a mirror: what it replaced, removed and left alone.
    pub mirrored: Option<Mirrored>,
    /// For a check of contents: how many files were the same, and not.
    pub compared: Option<super::Compared>,
    /// What the transfer meant to bring and how much of it is at the
    /// destination; `None` where it isn't counted (mirrors, copies through
    /// the Mac).
    pub arrival: Option<Arrival>,
    /// What became of each file, in the order copied (downloads, moves to
    /// the Mac and imports; empty for the rest). At most `REPORT_MOST`.
    pub report: Vec<FileReport>,
    /// Files past `REPORT_MOST`, left out of `report`.
    pub report_left_out: usize,
    /// How long it ran, start to end, as the app's queue timed it (the
    /// services leave it at zero).
    pub elapsed: Duration,
}

/// The most files a transfer's report lists: a million-file plan would
/// otherwise hold its report in memory twice over.
pub const REPORT_MOST: usize = 100_000;

/// One file of a transfer, as its report lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// As it is named at the destination, relative to it.
    pub path: String,
    /// Its own name where it came from, relative to the folder it was
    /// chosen in, when that is not `path` (saved as “x (1)”).
    pub from: Option<String>,
    pub outcome: FileOutcome,
    /// For a move (or an import that frees the phone): what became of the
    /// original.
    pub original: Option<Original>,
}

/// What became of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    /// Copied; `checked` when it is known to be whole (exactly the size the
    /// phone gave).
    Copied { checked: bool },
    /// Carried on from what an earlier try left, and finished.
    Resumed { checked: bool },
    /// Sent to the phone, which took it; not read back (a plain upload).
    Sent,
    /// The same file (size and date) was there already.
    AlreadyThere,
    /// Put in place of a different file of its name, which went to the Trash.
    Replaced,
    /// Sent in place of a different file of its name on the phone, which the
    /// phone deleted (for good: a phone has no Trash).
    ReplacedOnPhone,
    /// Left alone because its name was taken (the user chose Skip, or the
    /// phone's copy was not newer).
    Skipped,
    /// Could not be copied, and why.
    Failed(Error),
    /// Not copied, with nothing more to say (a step after the batch stopped).
    NotCopied,
    /// Checked: the phone's bytes and the Mac's are the same.
    Identical,
    /// Checked: the first byte that differs, counted from 0 (where the
    /// shorter one ends, when one is the start of the other).
    Differs { at: u64 },
    /// Checked without reading: the two files' sizes differ.
    SizesDiffer { phone: u64, mac: u64 },
    /// Checked: on the phone, but not on the Mac (or a folder there).
    NotOnMac,
    /// Checked: a selected name that isn't on the phone.
    NotOnPhone,
}

/// What became of an original after a move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Original {
    Removed,
    /// Left where it was, and why.
    Kept(Why),
    /// Left where it was: not copied, inside something that stayed, or not
    /// reached before the removal stopped.
    Stayed,
}

/// The selected files, counted at the end: so many were meant to arrive,
/// so many are there (copied now, or found already there), and of those so
/// many were checked to be whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Arrival {
    /// Files the transfer was to bring (names the user chose to skip are not
    /// among them), and their bytes where the sizes are known.
    pub files: usize,
    pub bytes: u64,
    /// Of those files, how many are at the destination.
    pub there: usize,
    /// Of those, how many were checked to be exactly the size of the
    /// original; `None` when nothing was read back (a plain copy to the
    /// phone, which the phone accepted file by file).
    pub checked: Option<usize>,
    /// Every file under the selection was seen. A folder the phone listed
    /// inside itself was not walked again, so "all" can't be said.
    pub complete: bool,
}

/// What a move did with its originals. Only a file whose copy was checked
/// (going to the Mac: it is there in exactly the size the phone reported;
/// going to the phone: the phone reports the size that was sent) has its
/// original removed; everything else stays where it was.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Removal {
    /// The originals were Mac items, and what was removed went to the Trash (a
    /// move to the phone). Otherwise they were on the phone.
    pub from_mac: bool,
    /// Files removed (from the phone, or in the items sent to the Trash).
    pub removed: usize,
    /// The bytes those files held on the phone, where it counts them (an
    /// import that frees the phone says how much).
    pub freed: u64,
    /// Originals left alone, and why. A file that did not copy is listed in
    /// `TransferSummary::failures` and not again here.
    pub kept: Vec<Kept>,
    /// The selected items still where they were, whole or in part: what a retry
    /// has left to move. On the phone, names in the folder they came from; on
    /// the Mac, the paths as they were given.
    pub left: Vec<String>,
    /// Removing ended early because the phone stopped answering, or the user
    /// cancelled; whatever was not reached was left alone.
    pub stopped: Option<Error>,
}

/// An original that stayed on the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// The item, relative to the folder it was copied from.
    pub label: String,
    pub why: Why,
}

/// Why an original was not removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The copy could not be checked: the phone does not say how big the file
    /// is (some phones give no size for 4 GB or more).
    Unchecked,
    /// It changed on the phone after it was copied (or was re-keyed), so the
    /// copy may not be what is there now.
    Changed,
    /// A folder that still holds something: something new, or something that
    /// was not copied.
    NotEmpty,
    /// The phone, or the Trash, refused to remove it.
    Refused(Error),
    /// It was never sent: a link to a folder, or something that is neither a
    /// file nor a folder, which uploads leave out.
    NotCopied,
}

/// One file (or folder) a batch could not copy, so the rest of the batch
/// could carry on without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFailure {
    /// The file, relative to the destination.
    pub label: String,
    pub error: Error,
}

/// Shared stop switch for a running transfer. Cloning shares the switch.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_is_seen_by_every_clone() {
        let cancel = Cancel::new();
        let shared = cancel.clone();
        assert!(!shared.is_cancelled());

        cancel.cancel();

        assert!(shared.is_cancelled());
    }
}

/// Delete `names` in the phone folder `folder`, keeping a copy of each in the
/// Mac's Trash first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRequest {
    pub storage_id: u64,
    pub folder: String,
    pub names: Vec<String>,
}

/// The most a delete copies to the Trash first; over it, a phone delete is
/// for good, as it always was.
pub const KEEP_MOST: u64 = 2_000_000_000;
