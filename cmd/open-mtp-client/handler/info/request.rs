/// What the user did in the Get Info dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Done, Escape, or ⌘I again: the dialog goes and any count stops.
    Close,
    /// A kind in what fills it: find those files (`app.rs`: the phone's
    /// search ordered by size, or the Mac pane's kind filter).
    ShowKind(internal::dto::SpaceKind),
}
