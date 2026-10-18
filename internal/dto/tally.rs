/// What is inside a folder (or a selection), counted: Get Info's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub files: usize,
    pub folders: usize,
    /// The files' bytes, where their sizes are known.
    pub bytes: u64,
    /// Files whose size the phone does not give (4 GB or more, as MTP says).
    pub unknown_sizes: usize,
    /// Folders that could not be read (on the Mac, ones macOS keeps private):
    /// what is in them is not counted.
    pub unreadable: usize,
    /// The count stopped at the most it looks at: there is more.
    pub incomplete: bool,
    /// The known bytes split by kind, in `SpaceKind::ALL`'s order.
    pub by_kind: [u64; 7],
}

/// What a file's bytes count as when a folder is split by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceKind {
    Photos,
    Videos,
    Music,
    Documents,
    Archives,
    /// Android apps (.apk, .apks, .xapk), as the pane's badges have them.
    Apps,
    Other,
}

impl SpaceKind {
    pub const ALL: [SpaceKind; 7] = [
        SpaceKind::Photos,
        SpaceKind::Videos,
        SpaceKind::Music,
        SpaceKind::Documents,
        SpaceKind::Archives,
        SpaceKind::Apps,
        SpaceKind::Other,
    ];

    /// What a file named `name` counts as.
    pub fn of(name: &str) -> Self {
        let extension = name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(extension.as_str(), "apk" | "apks" | "xapk") {
            return SpaceKind::Apps;
        }
        match super::FileKind::of(name) {
            Some(super::FileKind::Photo) => SpaceKind::Photos,
            Some(super::FileKind::Video) => SpaceKind::Videos,
            Some(super::FileKind::Music) => SpaceKind::Music,
            Some(super::FileKind::Document) => SpaceKind::Documents,
            Some(super::FileKind::Archive) => SpaceKind::Archives,
            None => SpaceKind::Other,
        }
    }

    /// The search's and the filter's kind for it; apps and the rest have none.
    pub fn file_kind(self) -> Option<super::FileKind> {
        match self {
            SpaceKind::Photos => Some(super::FileKind::Photo),
            SpaceKind::Videos => Some(super::FileKind::Video),
            SpaceKind::Music => Some(super::FileKind::Music),
            SpaceKind::Documents => Some(super::FileKind::Document),
            SpaceKind::Archives => Some(super::FileKind::Archive),
            SpaceKind::Apps | SpaceKind::Other => None,
        }
    }

    fn index(self) -> usize {
        SpaceKind::ALL
            .iter()
            .position(|kind| *kind == self)
            .unwrap_or(SpaceKind::ALL.len() - 1)
    }
}

impl Tally {
    /// Both counts together, as a selection of several items adds up.
    pub fn add(&mut self, other: &Tally) {
        self.files += other.files;
        self.folders += other.folders;
        self.bytes += other.bytes;
        self.unknown_sizes += other.unknown_sizes;
        self.unreadable += other.unreadable;
        self.incomplete |= other.incomplete;
        for (mine, theirs) in self.by_kind.iter_mut().zip(other.by_kind) {
            *mine += theirs;
        }
    }

    /// Counts one file named `name`: its bytes when `size` is known (and in
    /// its kind's share), else as one of unknown size.
    pub fn add_file(&mut self, name: &str, size: Option<u64>) {
        self.files += 1;
        match size {
            Some(size) => {
                self.bytes += size;
                self.by_kind[SpaceKind::of(name).index()] += size;
            }
            None => self.unknown_sizes += 1,
        }
    }

    /// The known bytes of files of `kind`.
    pub fn bytes_of(&self, kind: SpaceKind) -> u64 {
        self.by_kind[kind.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_counted_by_kind_and_unknown_sizes_apart() {
        let mut tally = Tally::default();
        tally.add_file("IMG_1.JPG", Some(3_000));
        tally.add_file("clip.mp4", Some(40_000));
        tally.add_file("game.apk", Some(500));
        tally.add_file("notes", Some(7));
        tally.add_file("film.mkv", None);
        assert_eq!(
            (tally.files, tally.bytes, tally.unknown_sizes),
            (5, 43_507, 1)
        );
        assert_eq!(tally.bytes_of(SpaceKind::Photos), 3_000);
        assert_eq!(
            tally.bytes_of(SpaceKind::Videos),
            40_000,
            "the unknown one isn't in it"
        );
        assert_eq!(tally.bytes_of(SpaceKind::Apps), 500);
        assert_eq!(tally.bytes_of(SpaceKind::Other), 7);
        let mut both = tally;
        both.add(&tally);
        assert_eq!(both.bytes_of(SpaceKind::Videos), 80_000);
        assert_eq!(SpaceKind::Apps.file_kind(), None);
        assert_eq!(
            SpaceKind::Music.file_kind(),
            Some(crate::dto::FileKind::Music)
        );
        assert_eq!(SpaceKind::of("archive.ZIP"), SpaceKind::Archives);
        assert_eq!(SpaceKind::of("thesis.pdf"), SpaceKind::Documents);
    }
}
