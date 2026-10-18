use super::FileFailure;

/// What pasting does with the items that were copied or cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paste {
    /// The originals stay.
    Copy,
    /// The originals move: they are no longer where they were.
    Move,
}

/// Copy or move `names` from the phone folder `from` (on storage `from_storage`)
/// into the phone folder `to` (on `to_storage`). Everything stays on the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteRequest {
    pub mode: Paste,
    pub from_storage: u64,
    pub from: String,
    pub names: Vec<String>,
    pub to_storage: u64,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteSummary {
    pub mode: Paste,
    /// Items copied or moved.
    pub done: usize,
    /// Of those, items that arrived under a new name because theirs was taken.
    pub renamed: usize,
    /// The folder they went into.
    pub into: String,
    /// What the items are called there, in the order they were done.
    pub names: Vec<String>,
    /// Items that could not be done while the rest went on.
    pub failures: Vec<FileFailure>,
}
