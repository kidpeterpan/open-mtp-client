/// The banner below the panes, worded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    pub hint: String,
    /// There is a report file to open.
    pub can_show: bool,
}
