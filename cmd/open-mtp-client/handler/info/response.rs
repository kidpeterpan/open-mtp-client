/// Get Info, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoResponse {
    /// "IMG_0001.jpg", "3 items", or the folder's own name.
    pub title: String,
    /// Label and value, in order: Kind, Where, Size, Items, Modified.
    pub rows: Vec<(String, String)>,
    /// Lines under the rows: what the count includes, what it could not see.
    pub notes: Vec<String>,
    /// What fills it, by kind, the most first; empty for a single file.
    pub kinds: Vec<KindShare>,
}

/// One kind's part of what was counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindShare {
    pub kind: internal::dto::SpaceKind,
    /// "Videos 41.2 GB".
    pub label: String,
    /// Its part of the bar, in thousandths (at least 1).
    pub portion: u16,
    /// A click finds these files (apps and the rest have no kind to find by).
    pub findable: bool,
}
