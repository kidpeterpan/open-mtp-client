/// One result, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRow {
    /// Its full path on the phone, which tells it from a file of the same
    /// name in another folder.
    pub key: String,
    /// "IMG_0001.jpg", or "Camera/" for a folder.
    pub name: String,
    /// The folder it is in: "DCIM › Camera", or "Top folder".
    pub place: String,
    pub size: String,
    pub modified: String,
}
