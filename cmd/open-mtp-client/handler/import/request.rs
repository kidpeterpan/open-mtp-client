/// What the user asked the import dialog to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// A folder for each day, month or year, or none.
    Folders(internal::dto::ImportFolders),
    /// Remove from the phone what was copied and checked.
    RemoveAfter(bool),
    /// Queue the import.
    Import,
    Close,
}
