//! Searching the whole phone: what is looked for, and what is found.

use super::{Entry, Timestamp};

/// What kind of file a search can ask for, told by the name's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Photo,
    Video,
    Music,
    Document,
    Archive,
}

impl FileKind {
    pub const ALL: [FileKind; 5] = [
        FileKind::Photo,
        FileKind::Video,
        FileKind::Music,
        FileKind::Document,
        FileKind::Archive,
    ];

    /// The kind of file `name` is, from its extension (any case), if it is one
    /// of the kinds a search knows.
    pub fn of(name: &str) -> Option<FileKind> {
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty() {
            // ".nomedia": a hidden file with no extension.
            return None;
        }
        let kind = match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "heic" | "heif" | "gif" | "webp" | "dng" | "bmp" | "tif"
            | "tiff" | "raw" | "cr2" | "nef" | "arw" | "avif" => FileKind::Photo,
            "mp4" | "mov" | "m4v" | "3gp" | "3g2" | "mkv" | "webm" | "avi" | "wmv" | "ts"
            | "mts" | "m2ts" | "flv" => FileKind::Video,
            "mp3" | "m4a" | "aac" | "flac" | "wav" | "ogg" | "oga" | "opus" | "wma" | "amr"
            | "aiff" | "aif" | "mid" | "midi" => FileKind::Music,
            "pdf" | "doc" | "docx" | "txt" | "rtf" | "odt" | "xls" | "xlsx" | "ods" | "csv"
            | "ppt" | "pptx" | "odp" | "md" | "epub" | "pages" | "numbers" | "key" | "html"
            | "htm" | "json" | "xml" => FileKind::Document,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "apk" | "apks"
            | "xapk" | "obb" | "iso" => FileKind::Archive,
            _ => return None,
        };
        Some(kind)
    }
}

/// A calendar day, as a date filter compares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Timestamp {
    /// The day of this time, as it is shown.
    pub fn day(&self) -> Day {
        Day {
            year: self.year,
            month: self.month,
            day: self.day,
        }
    }
}

/// The days a date filter keeps, both ends included; an open end keeps
/// every day that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DaySpan {
    pub from: Option<Day>,
    pub until: Option<Day>,
}

impl DaySpan {
    pub fn holds(&self, day: Day) -> bool {
        self.from.is_none_or(|from| day >= from) && self.until.is_none_or(|until| day <= until)
    }
}

/// The dates a filter offers, counted back from today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateRange {
    Today,
    Yesterday,
    /// Today and the six days before.
    LastWeek,
    /// Today and the 29 days before.
    LastMonth,
    /// Since the first of January.
    ThisYear,
    /// Before this day a year ago.
    OlderThanAYear,
}

impl DateRange {
    pub const ALL: [DateRange; 6] = [
        DateRange::Today,
        DateRange::Yesterday,
        DateRange::LastWeek,
        DateRange::LastMonth,
        DateRange::ThisYear,
        DateRange::OlderThanAYear,
    ];
}

/// What a search looks for: names holding every word of `text` (in any case),
/// of `kind` if one is given, changed on a day `date` holds if one is given.
/// Folders are found by name, never by kind or date.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub text: String,
    pub kind: Option<FileKind>,
    pub date: Option<DaySpan>,
}

impl SearchQuery {
    /// Nothing to look for: no words, no kind and no date.
    pub fn is_empty(&self) -> bool {
        self.kind.is_none() && self.date.is_none() && self.text.trim().is_empty()
    }

    /// Whether `entry` is what is looked for. Ask `matcher` once for a query
    /// that is run over many entries; it does the lower-casing once.
    pub fn matches(&self, entry: &Entry) -> bool {
        self.matcher().matches(entry)
    }

    pub fn matcher(&self) -> Matcher {
        Matcher {
            words: self
                .text
                .split_whitespace()
                .map(str::to_lowercase)
                .collect(),
            kind: self.kind,
            date: self.date,
            empty: self.is_empty(),
        }
    }
}

/// A query made ready to run over many names.
#[derive(Debug, Clone)]
pub struct Matcher {
    words: Vec<String>,
    kind: Option<FileKind>,
    date: Option<DaySpan>,
    empty: bool,
}

impl Matcher {
    pub fn matches(&self, entry: &Entry) -> bool {
        if self.empty {
            return false;
        }
        if let Some(kind) = self.kind
            && (entry.is_folder || FileKind::of(&entry.name) != Some(kind))
        {
            return false;
        }
        if let Some(span) = self.date
            && (entry.is_folder || !entry.modified.is_some_and(|at| span.holds(at.day())))
        {
            return false;
        }
        if self.words.is_empty() {
            return true;
        }
        let name = entry.name.to_lowercase();
        self.words.iter().all(|word| name.contains(word.as_str()))
    }
}

/// Something on the phone, and the folder it is in (an absolute path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub folder: String,
    pub entry: Entry,
}

/// A piece of a walk of the whole phone: what was seen since the last piece,
/// and how far along it is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WalkUpdate {
    /// Everything seen since the last piece, files and folders.
    pub found: Vec<Found>,
    /// The walk started over (the phone re-keyed its files part way): drop
    /// what came before.
    pub restart: bool,
    pub folders_read: usize,
    /// Folders found but not read yet.
    pub folders_left: usize,
    /// Items seen so far.
    pub items: usize,
    pub done: bool,
    /// It stopped at the most it will look at: there may be more.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> Entry {
        Entry {
            id: 1,
            name: name.to_owned(),
            is_folder: false,
            size_bytes: 1,
            modified: None,
        }
    }

    fn folder(name: &str) -> Entry {
        Entry {
            is_folder: true,
            ..file(name)
        }
    }

    #[test]
    fn a_kind_is_told_by_the_extension_in_any_case() {
        for (name, kind) in [
            ("IMG_0001.JPG", Some(FileKind::Photo)),
            ("PXL_2026.heic", Some(FileKind::Photo)),
            ("VID_1.mp4", Some(FileKind::Video)),
            ("clip.MOV", Some(FileKind::Video)),
            ("song.flac", Some(FileKind::Music)),
            ("Voice 001.m4a", Some(FileKind::Music)),
            ("report.PDF", Some(FileKind::Document)),
            ("game.nsp.rar", Some(FileKind::Archive)),
            ("app.apk", Some(FileKind::Archive)),
            ("notes", None),
            (".nomedia", None),
            ("thing.xyz", None),
            ("trailing.", None),
        ] {
            assert_eq!(FileKind::of(name), kind, "{name}");
        }
    }

    #[test]
    fn every_word_must_be_in_the_name_in_any_case() {
        let query = SearchQuery {
            text: "  beach   2026 ".into(),
            ..SearchQuery::default()
        };

        assert!(query.matches(&file("Beach day 2026.jpg")));
        assert!(query.matches(&file("2026-BEACH.mp4")));
        assert!(!query.matches(&file("beach.jpg")), "2026 is missing");
        assert!(query.matches(&folder("Beach 2026")), "folders by name too");
    }

    #[test]
    fn a_kind_finds_files_of_that_kind_and_never_a_folder() {
        let videos = SearchQuery {
            kind: Some(FileKind::Video),
            ..SearchQuery::default()
        };

        assert!(videos.matches(&file("a.mp4")));
        assert!(!videos.matches(&file("a.jpg")));
        assert!(!videos.matches(&folder("Movies.mp4")));
        let beach_videos = SearchQuery {
            text: "beach".into(),
            ..videos
        };
        assert!(beach_videos.matches(&file("beach.mov")));
        assert!(!beach_videos.matches(&file("city.mov")));
        assert!(!beach_videos.matches(&file("beach.jpg")));
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        let empty = SearchQuery {
            text: "   ".into(),
            ..SearchQuery::default()
        };

        assert!(empty.is_empty());
        assert!(!empty.matches(&file("anything.jpg")));
    }

    fn day(year: u16, month: u8, day: u8) -> Day {
        Day { year, month, day }
    }

    fn changed(name: &str, on: Day) -> Entry {
        Entry {
            modified: Some(Timestamp {
                year: on.year,
                month: on.month,
                day: on.day,
                hour: 23,
                minute: 59,
            }),
            ..file(name)
        }
    }

    #[test]
    fn a_date_keeps_files_changed_on_a_day_it_holds_and_never_a_folder() {
        let june = SearchQuery {
            date: Some(DaySpan {
                from: Some(day(2026, 6, 1)),
                until: Some(day(2026, 6, 30)),
            }),
            ..SearchQuery::default()
        };
        assert!(!june.is_empty(), "a date alone is something to look for");

        assert!(
            june.matches(&changed("first.jpg", day(2026, 6, 1))),
            "ends included"
        );
        assert!(june.matches(&changed("last.jpg", day(2026, 6, 30))));
        assert!(!june.matches(&changed("before.jpg", day(2026, 5, 31))));
        assert!(!june.matches(&changed("after.jpg", day(2026, 7, 1))));
        assert!(!june.matches(&file("undated.jpg")), "no date: not kept");
        assert!(!june.matches(&Entry {
            is_folder: true,
            ..changed("Camera", day(2026, 6, 15))
        }));

        let june_photos_of_the_beach = SearchQuery {
            text: "beach".into(),
            kind: Some(FileKind::Photo),
            ..june.clone()
        };
        assert!(june_photos_of_the_beach.matches(&changed("beach.jpg", day(2026, 6, 9))));
        assert!(!june_photos_of_the_beach.matches(&changed("beach.mp4", day(2026, 6, 9))));
        assert!(!june_photos_of_the_beach.matches(&changed("city.jpg", day(2026, 6, 9))));

        let open = DaySpan {
            from: None,
            until: Some(day(2025, 6, 14)),
        };
        assert!(
            open.holds(day(1999, 1, 1)),
            "an open end keeps everything that way"
        );
        assert!(!open.holds(day(2025, 6, 15)));
        assert!(DaySpan::default().holds(day(2026, 1, 1)));
    }
}
