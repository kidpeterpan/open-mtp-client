/// Where an import puts each photo, by the date the phone gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportFolders {
    /// Straight into the destination.
    #[default]
    None,
    /// A folder a day: "2026-10-17".
    Day,
    /// A folder a month: "2026-10".
    Month,
    /// A folder a year, a folder a month in it: "2026/10".
    YearMonth,
}

/// Bring a phone's new photos and videos over to the Mac.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    /// The storage whose DCIM folder is imported.
    pub storage_id: u64,
    /// The Mac folder they go into.
    pub destination: String,
    /// Into a folder for each day, month or year, by the date the phone
    /// gives, or straight into the destination.
    pub folders: ImportFolders,
    /// Afterwards, remove from the phone each photo and video whose copy was
    /// checked, to free its room.
    pub remove_from_phone: bool,
}

/// What an import would do, worked out before anything is copied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportPreview {
    /// Photos and videos to copy.
    pub new_files: usize,
    /// Their size, less what earlier attempts already brought over.
    pub new_bytes: u64,
    /// Photos and videos that are on the Mac already, or were imported from
    /// this phone before (and may have been moved or deleted since).
    pub already: usize,
}
