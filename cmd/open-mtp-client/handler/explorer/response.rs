//! Display-ready view models: every string here is shown as-is.

#[derive(Debug, Clone)]
pub struct EntryResponse {
    pub name: String,
    pub is_folder: bool,
    pub kind: String,
    pub size: String,
    pub modified: String,
    /// A file's kind at a glance, for its glyph and a tile with no
    /// thumbnail; `None` for a folder.
    pub badge: Option<Badge>,
}

/// What kind of file an entry is, as its badge shows it: a colour for the
/// kind and a short label from its extension ("MP4"; empty with none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub kind: BadgeKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    Photo,
    Video,
    Music,
    Document,
    Archive,
    /// An Android app to install (.apk and friends).
    App,
    Other,
}

impl BadgeKind {
    /// The word under a tile's badge.
    pub fn word(self) -> &'static str {
        match self {
            BadgeKind::Photo => "Photo",
            BadgeKind::Video => "Video",
            BadgeKind::Music => "Music",
            BadgeKind::Document => "Document",
            BadgeKind::Archive => "Archive",
            BadgeKind::App => "Android app",
            BadgeKind::Other => "File",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrumbResponse {
    pub label: String,
    pub path: String,
}

/// What a paste says when it is over, shown as a banner above the file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeResponse {
    /// "Copied 3 items".
    pub title: String,
    /// Lines under it: what was renamed, what could not be done.
    pub details: Vec<String>,
    /// Something went wrong, so the banner is drawn as a problem.
    pub problem: bool,
}

/// The result of a paste: what to select in the folder, and what to tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteResponse {
    pub select: Vec<String>,
    pub notice: NoticeResponse,
    /// How to undo it: a whole move inside the volume, else `None`.
    pub undo: Option<super::undo::Undo>,
}
