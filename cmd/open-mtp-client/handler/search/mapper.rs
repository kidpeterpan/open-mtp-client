use chrono::NaiveDate;
use internal::dto;

use super::response::ResultRow;
use crate::handler::format::{format_bytes, format_count, format_timestamp};
use crate::i18n::{count, t, tf};

/// A result's full path, which tells it from a file of the same name in
/// another folder.
pub fn key_of(found: &dto::Found) -> String {
    if found.folder == "/" {
        format!("/{}", found.entry.name)
    } else {
        format!("{}/{}", found.folder, found.entry.name)
    }
}

pub fn to_row(found: &dto::Found, today: NaiveDate) -> ResultRow {
    let entry = &found.entry;
    ResultRow {
        key: key_of(found),
        name: if entry.is_folder {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        },
        place: place_of(&found.folder),
        size: if entry.is_folder {
            "—".to_owned()
        } else {
            format_bytes(entry.size_bytes)
        },
        modified: entry
            .modified
            .as_ref()
            .map_or_else(|| "—".to_owned(), |time| format_timestamp(time, today)),
    }
}

/// "DCIM › Camera" for "/DCIM/Camera"; "Top folder" for the storage's root.
pub fn place_of(folder: &str) -> String {
    let parts: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        t("search.top_folder").to_owned()
    } else {
        parts.join(" › ")
    }
}

pub use crate::handler::format::kind_label;

/// Where a search is, in a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub folders_read: usize,
    pub items: usize,
    pub done: bool,
    pub truncated: bool,
}

pub fn status(progress: Progress, asked: bool, found: usize, storage: &str) -> String {
    let found_text = match found {
        0 => t("search.found_none_yet").to_owned(),
        n => count("search.found", n, &[]),
    };
    if !progress.done {
        let seen = tf(
            "search.looking",
            &[
                ("storage", storage),
                ("folders", &format_count(progress.folders_read)),
                ("items", &format_count(progress.items)),
            ],
        );
        return if asked {
            tf("text.dot", &[("first", &seen), ("second", &found_text)])
        } else {
            seen
        };
    }
    let line = if !asked {
        tf(
            "search.idle",
            &[
                ("items", &format_count(progress.items)),
                ("storage", storage),
            ],
        )
    } else {
        match found {
            0 => tf("search.none", &[("storage", storage)]),
            n => count("search.found_on", n, &[("storage", storage)]),
        }
    };
    if progress.truncated {
        let truncated = tf(
            "search.truncated",
            &[("items", &format_count(progress.items))],
        );
        return tf("text.dot", &[("first", &line), ("second", &truncated)]);
    }
    line
}

/// "Download to Mac", "Download 3 to Mac".
pub fn download_label(selected: usize) -> String {
    match selected {
        0 | 1 => t("search.download_one").to_owned(),
        n => count("search.download_many", n, &[]),
    }
}

/// The order after a click on a column: the same column turns round; another
/// starts A→Z for names, and largest or newest first for size and date.
pub fn next_order(
    current: Option<(dto::SortKey, bool)>,
    key: dto::SortKey,
) -> (dto::SortKey, bool) {
    match current {
        Some((now, descending)) if now == key => (key, !descending),
        _ => (key, key != dto::SortKey::Name),
    }
}

/// A column's heading, with an arrow on the one the results are ordered by.
pub fn order_label(title: &str, key: dto::SortKey, order: Option<(dto::SortKey, bool)>) -> String {
    match order {
        Some((now, false)) if now == key => format!("{title} ▲"),
        Some((now, true)) if now == key => format!("{title} ▼"),
        _ => title.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(folder: &str, name: &str, is_folder: bool) -> dto::Found {
        dto::Found {
            folder: folder.into(),
            entry: dto::Entry {
                id: 1,
                name: name.into(),
                is_folder,
                size_bytes: 2_500_000,
                modified: Some(dto::Timestamp {
                    year: 2026,
                    month: 3,
                    day: 4,
                    hour: 9,
                    minute: 5,
                }),
            },
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
    }

    #[test]
    fn a_result_shows_its_name_where_it_is_and_its_size() {
        let row = to_row(&found("/DCIM/Camera", "IMG_1.jpg", false), today());

        assert_eq!(row.key, "/DCIM/Camera/IMG_1.jpg");
        assert_eq!(row.name, "IMG_1.jpg");
        assert_eq!(row.place, "DCIM › Camera");
        assert_eq!(row.size, "2.5 MB");
        assert_eq!(row.modified, "4 Mar, 09:05");
    }

    #[test]
    fn a_folder_or_something_at_the_top_says_so() {
        let row = to_row(&found("/", "Camera", true), today());

        assert_eq!(row.key, "/Camera");
        assert_eq!(row.name, "Camera/");
        assert_eq!(row.place, "Top folder");
        assert_eq!(row.size, "—");
    }

    #[test]
    fn the_status_says_how_far_the_search_is_and_what_it_found() {
        let walking = Progress {
            folders_read: 1_200,
            items: 34_567,
            ..Progress::default()
        };
        assert_eq!(
            status(walking, true, 12, "Internal"),
            "Looking through Internal: 1,200 folders, 34,567 items so far · 12 found"
        );
        assert_eq!(
            status(walking, false, 0, "Internal"),
            "Looking through Internal: 1,200 folders, 34,567 items so far"
        );
        let done = Progress {
            done: true,
            ..walking
        };
        assert_eq!(status(done, true, 1, "Internal"), "1 found on Internal");
        assert_eq!(
            status(done, true, 0, "Internal"),
            "Nothing found on Internal"
        );
        assert_eq!(
            status(done, false, 0, "Internal"),
            "34,567 items on Internal: type part of a name, or choose a kind"
        );
        let cut = Progress {
            truncated: true,
            ..done
        };
        assert_eq!(
            status(cut, true, 3, "SD card"),
            "3 found on SD card · stopped after 34,567 items, so there may be more"
        );
    }

    #[test]
    fn labels() {
        assert_eq!(kind_label(None), "All");
        assert_eq!(kind_label(Some(dto::FileKind::Video)), "Videos");
        assert_eq!(download_label(0), "Download to Mac");
        assert_eq!(download_label(1_234), "Download 1,234 to Mac");
    }
}
