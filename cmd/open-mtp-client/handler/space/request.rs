/// What the user asked the Free Up Space dialog to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Picks or unpicks one file.
    Toggle(String),
    /// Picks every copy but the one to keep in each group.
    SelectCopies,
    /// Picks every photo already on this Mac.
    SelectImported,
    /// Unpicks everything.
    SelectNone,
    /// Asks before removing what is picked.
    Remove,
    /// Removes what is picked, for good.
    ConfirmRemove,
    /// Back from the question, nothing removed.
    KeepThem,
    /// Looks at the phone again.
    LookAgain,
    /// The phone's biggest files: the search, ordered by size (`app.rs`).
    BiggestFiles,
    Close,
}
