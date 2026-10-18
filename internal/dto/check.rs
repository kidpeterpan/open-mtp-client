//! Checking copies byte for byte: the phone's files against the Mac's of the
//! same name, read and compared, nothing written or changed on either side.

/// Compare `names` (files and folders, everything inside them) in the phone's
/// `phone_folder` with what has the same names in the Mac's `mac_folder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRequest {
    pub storage_id: u64,
    pub phone_folder: String,
    pub mac_folder: String,
    pub names: Vec<String>,
}

/// What a check found, file by file counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Compared {
    /// The same bytes on both sides.
    pub identical: usize,
    /// Different sizes, or a different byte.
    pub differ: usize,
    /// On one side only.
    pub missing: usize,
    /// Couldn't be read on one side (each is also in the failures).
    pub unreadable: usize,
}

impl Compared {
    /// Every file that was looked at.
    pub fn total(&self) -> usize {
        self.identical + self.differ + self.missing + self.unreadable
    }
}
