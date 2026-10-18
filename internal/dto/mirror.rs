/// Which way a mirror goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MirrorDirection {
    /// The phone folder's contents onto the Mac folder.
    #[default]
    ToMac,
    /// The Mac folder's contents onto the phone folder.
    ToPhone,
}

/// Make one folder's contents match the other's: copy what is new, replace
/// what changed, and remove what is only in the target when asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorRequest {
    pub storage_id: u64,
    pub phone_folder: String,
    pub mac_folder: String,
    pub direction: MirrorDirection,
    /// Remove what is only in the target (to the Trash on the Mac; for good
    /// on the phone).
    pub remove_extras: bool,
}

/// What a mirror would do, worked out before anything is touched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirrorPreview {
    pub new_files: usize,
    pub new_bytes: u64,
    pub changed_files: usize,
    pub changed_bytes: u64,
    /// Files and folders only in the target (a folder counts once).
    pub extras: usize,
    /// Left alone: names differing only in case, a file where a folder is.
    pub conflicts: usize,
    /// The first few of each, to show.
    pub samples: MirrorSamples,
}

/// The first `SAMPLE` paths of each kind, relative to the folders.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirrorSamples {
    pub new: Vec<String>,
    pub changed: Vec<String>,
    pub extras: Vec<String>,
    pub conflicts: Vec<String>,
}

/// How many paths of each kind a preview shows.
pub const MIRROR_SAMPLE: usize = 8;

/// What a mirror did besides copying new files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mirrored {
    /// New files copied.
    pub copied: usize,
    /// Files replaced by the source's version.
    pub replaced: usize,
    /// Extras removed (to the Trash on the Mac).
    pub removed: usize,
    /// Extras left where they were (not asked to remove them).
    pub kept: usize,
    /// Left alone because of a clash of names or kinds.
    pub conflicts: usize,
}
