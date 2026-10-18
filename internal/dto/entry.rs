/// One file or folder inside a device folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The phone's object id, for fetching its thumbnail without listing the
    /// folder again; 0 on the Mac.
    pub id: u64,
    pub name: String,
    pub is_folder: bool,
    pub size_bytes: u64,
    pub modified: Option<Timestamp>,
}

impl Entry {
    /// Dot-files, the rule behind the "hide hidden files" setting.
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// A small preview image: `width × height` RGBA pixels, at most
/// `THUMBNAIL_SIZE` on either side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Made for the biggest grid tile (`TileSize`), shrunk on screen for the rest.
pub const THUMBNAIL_SIZE: u32 = 192;

/// Wall-clock time as the device wrote it; MTP rarely records a time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}
