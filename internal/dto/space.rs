//! Freeing space on the phone: what one walk of a storage finds that could
//! go (copies of the same file, photos already imported to this Mac), and
//! removing the files the user picks.

use super::Timestamp;

/// A file on the phone, as the space view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceFile {
    /// Its path on the storage, "/DCIM/Camera/IMG_1.jpg".
    pub path: String,
    pub size: u64,
    /// The date the pane shows for it.
    pub modified: Option<Timestamp>,
}

/// Files whose whole contents are the same, read in full to know it; the
/// first is the one to keep (the oldest, or the camera's own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub size: u64,
    pub files: Vec<SpaceFile>,
}

/// What a storage holds that could go.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpaceReport {
    pub storage_id: u64,
    /// Files walked.
    pub files: usize,
    pub duplicates: Vec<DuplicateGroup>,
    /// Photos and videos under DCIM whose imported copy is still on this Mac
    /// where the import put it, with the same size and date.
    pub imported: Vec<SpaceFile>,
}

impl SpaceReport {
    /// What removing every copy but the first in each group would free.
    pub fn duplicate_bytes(&self) -> u64 {
        self.duplicates
            .iter()
            .map(|group| group.size * (group.files.len() as u64 - 1))
            .sum()
    }
}

/// How far a look at a storage has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceProgress {
    /// Walking the storage: files seen so far.
    Walking { files: usize },
    /// Reading files of the same size to tell copies from strangers.
    Comparing { done: u64, total: u64 },
}

/// Remove these files from the phone, each only if it is still as the look
/// found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeUpRequest {
    pub storage_id: u64,
    pub paths: Vec<String>,
}
