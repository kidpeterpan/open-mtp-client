/// What the user asked the transfer panel to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Stop the running transfer, or drop the pending question, or the
    /// paused transfer.
    Cancel,
    /// Stop the running transfer where it can carry on from, and hold the
    /// queue until it is resumed or cancelled.
    Pause,
    /// Carry on with the paused transfer.
    Resume,
    /// Answer "already exists on the phone" and start the upload.
    Resolve(internal::dto::Conflict),
    /// Take a waiting transfer (by its place in the queue) out of the queue.
    Remove(usize),
    /// Queue a finished transfer (by its place in the history) again.
    Retry(usize),
    /// Open Finder on what a finished download copied.
    ShowInFinder(usize),
    /// Forget finished transfers; hides the panel when nothing is left.
    Clear,
    /// Show what became of each file of a finished transfer (by its place in
    /// the history), in the finished list's place.
    Details(usize),
    /// Back to the finished list.
    CloseDetails,
    /// Write the report shown into the folder the files went to.
    SaveReport,
    /// Put the report shown on the clipboard, as text a spreadsheet takes.
    CopyReport,
    /// Queue the transfers not finished last time.
    ContinueSaved,
    /// Send this working copy (by id) back to the phone.
    SendEdit(String),
    /// Forget this working copy and its edits.
    DiscardEdit(String),
    /// Forget them.
    DiscardSaved,
}
