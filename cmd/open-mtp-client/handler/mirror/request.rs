use internal::dto::MirrorDirection;

/// What the user asked the mirror dialog to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Direction(MirrorDirection),
    /// Also remove what is only in the target.
    RemoveExtras(bool),
    /// Queue the mirror.
    Start,
    /// Keep this mirror in the sidebar to run again (`app.rs` keeps it).
    Save,
    Close,
}
