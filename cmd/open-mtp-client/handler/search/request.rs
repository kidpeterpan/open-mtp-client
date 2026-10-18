use internal::dto::{DateRange, FileKind, SortKey};

/// What the user asks of the search panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The words typed so far.
    Query(String),
    /// Only files of this kind; `None` for any.
    Kind(Option<FileKind>),
    /// Only files changed on the days this keeps; `None` for any date.
    Date(Option<DateRange>),
    /// Order the results by this column (again: the other way).
    SortBy(SortKey),
    /// A click on the result with this key (its full path).
    Select(String),
    SelectAll,
    /// Download the selected results to the Mac pane's folder (the app does it).
    Download,
    /// Close the search and show the folder the result is in, with it selected
    /// (the app does it).
    ShowInFolder,
    /// Look through the phone again, as it is now.
    Refresh,
    Close,
}
