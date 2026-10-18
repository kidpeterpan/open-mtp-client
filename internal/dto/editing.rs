//! Editing a phone file in its Mac app: a working copy fetched from the
//! phone, watched for changes, and sent back when the user says so.

/// The most working copies at once.
pub const MOST_EDITS: usize = 20;

/// Edit `name` in the phone folder `folder` on the Mac.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    /// The phone it is on: its `Phone::key`.
    pub phone: String,
    pub storage_id: u64,
    pub folder: String,
    pub name: String,
}

/// A file's size and when it was last changed, to see an app save it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CopyState {
    pub size: u64,
    /// Milliseconds since 1970.
    pub modified_ms: u64,
}

/// A phone file fetched to be edited, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingCopy {
    /// Names its folder of the app's own.
    pub id: String,
    pub phone: String,
    pub storage_id: u64,
    pub folder: String,
    pub name: String,
    /// The copy on the Mac.
    pub path: String,
    /// The copy as it was fetched: anything else is an edit.
    pub fetched: CopyState,
    /// The phone's file as it was fetched: if it is still so, sending
    /// replaces it; if not, the edit goes in beside it.
    pub phone_size: u64,
    pub phone_date: String,
}
