use super::Entry;

/// One piece of a folder listing that arrives in pieces: a big camera folder
/// takes a phone one round trip per file to describe, so the files are shown as
/// they come instead of after the last one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListingUpdate {
    /// Files and folders to add to what is already shown (hidden ones already
    /// left out unless asked for).
    pub entries: Vec<Entry>,
    /// The phone changed the folder under the listing, which started over;
    /// forget what came before.
    pub restart: bool,
    /// How many items the phone has described so far, and how many there are.
    /// This counts hidden items too, so it reaches `total` even when fewer
    /// entries are shown.
    pub loaded: usize,
    pub total: usize,
    /// Nothing more is coming.
    pub done: bool,
}

/// One folder read from the phone and timed, for a support report: how many
/// items it holds (hidden ones too) and how long reading them took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadTiming {
    pub items: usize,
    pub took: std::time::Duration,
}
