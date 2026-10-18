#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    /// Folders, then photos, videos, music, documents, archives and the
    /// rest, each grouped by extension.
    Kind,
}

/// How a folder listing is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
    /// Keep folders above files whatever the key.
    pub folders_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            descending: false,
            folders_first: true,
        }
    }
}

/// The column a pane is sorted by and which way: what is remembered for each
/// pane (folders first is one setting for both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortOrder {
    pub key: SortKey,
    pub descending: bool,
}

/// What a pane's rows are grouped by under headers, as Finder's Use Groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    #[default]
    None,
    /// Today, Yesterday, Previous 7 Days, Previous 30 Days, each month of
    /// this year, then each year before.
    Date,
    /// Folders, then photos, videos, music, documents, archives, the rest.
    Kind,
    /// Folders, then from the biggest files to empty ones.
    Size,
}
