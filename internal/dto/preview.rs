/// Phone files to fetch to the Mac so that they can be looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRequest {
    pub storage_id: u64,
    /// The phone folder they are in.
    pub folder: String,
    pub names: Vec<String>,
}

/// What a fetch for a look brought over, and what it left on the phone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fetched {
    /// The Mac copies, in the order asked for.
    pub paths: Vec<String>,
    /// Too big to fetch just to look at (or of a size the phone doesn't say).
    pub too_big: Vec<String>,
    /// Folders, which are not fetched.
    pub folders: Vec<String>,
    /// More than `PREVIEW_MOST` were asked for: these were left out.
    pub left_out: usize,
    /// Could not be fetched.
    pub failed: Vec<String>,
}

/// The most files fetched for one look.
pub const PREVIEW_MOST: usize = 20;
/// The largest file fetched just to look at it.
pub const PREVIEW_LARGEST: u64 = 1_000_000_000;
/// How much the fetched copies may take up before the oldest are removed.
pub const PREVIEW_CACHE: u64 = 2 * 1024 * 1024 * 1024;

/// What a press of Space did to Quick Look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Opened,
    Closed,
}
