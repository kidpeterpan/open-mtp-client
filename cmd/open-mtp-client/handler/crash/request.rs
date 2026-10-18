/// What the user asked of the crash banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Open the report in the Mac's text editor.
    Show,
    /// Done with it: the banner goes, and the reports are marked as seen.
    Dismiss,
}
