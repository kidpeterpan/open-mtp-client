use chrono::NaiveDate;
use internal::dto;

use super::handler::{Place, Subject};
use super::response::InfoResponse;
use crate::handler::error::ErrorResponse;
use crate::handler::explorer;
use crate::handler::format::{
    abilities_in_words, comma_list, format_bytes, format_count, format_timestamp,
};
use crate::i18n::{count, english_only, t, tf};

/// MTP's size for "4 GB or more": not a size.
const UNKNOWN_SIZE: u64 = 0xFFFF_FFFF;

/// What the selection itself adds before anything is read: each selected file
/// and its size, and each selected folder (what is in it comes from a count).
pub fn base(items: &[dto::Entry]) -> dto::Tally {
    let mut tally = dto::Tally::default();
    for item in items {
        if item.is_folder {
            tally.folders += 1;
        } else if item.size_bytes == UNKNOWN_SIZE {
            tally.add_file(&item.name, None);
        } else {
            tally.add_file(&item.name, Some(item.size_bytes));
        }
    }
    tally
}

/// The folders whose insides are counted: the selected ones, or the folder
/// itself when nothing is selected.
pub fn folders_to_count(subject: &Subject) -> Vec<String> {
    if subject.items.is_empty() {
        return vec![subject.folder.clone()];
    }
    subject
        .items
        .iter()
        .filter(|item| item.is_folder)
        .map(|item| explorer::child_path(&subject.folder, &item.name))
        .collect()
}

/// What the dialog says, for `tally` (everything counted so far, the
/// selection included), while `counting` or once done, or after `error`.
pub fn to_info_response(
    subject: &Subject,
    tally: &dto::Tally,
    counting: bool,
    error: Option<&ErrorResponse>,
    today: NaiveDate,
) -> InfoResponse {
    let counts_folders = !folders_to_count(subject).is_empty();
    let mut rows = Vec::new();
    let mut row = |label: &str, value: String| rows.push((label.to_owned(), value));

    let title = match subject.items.as_slice() {
        [] => own_name(subject),
        [item] => item.name.clone(),
        items => count("text.items", items.len(), &[]),
    };

    row(
        t("label.column.kind"),
        match subject.items.as_slice() {
            [] => t("explorer.kind.folder").to_owned(),
            [item] => explorer::kind(item),
            items => {
                let folders = items.iter().filter(|item| item.is_folder).count();
                files_and_folders(items.len() - folders, folders)
            }
        },
    );
    if let Some(place) = where_it_is(subject) {
        row(t("info.where"), place);
    }

    let size = match subject.items.as_slice() {
        [item] if !item.is_folder => {
            if item.size_bytes == UNKNOWN_SIZE {
                t("info.size_unknown").to_owned()
            } else {
                exact_bytes(item.size_bytes)
            }
        }
        _ if error.is_some() => t("info.size_error").to_owned(),
        _ if counting => tf(
            "info.size_counting",
            &[
                ("files", &count("text.files", tally.files, &[])),
                ("bytes", &format_bytes(tally.bytes)),
            ],
        ),
        _ => exact_bytes(tally.bytes),
    };
    row(t("label.column.size"), size);
    if counts_folders && error.is_none() {
        row(
            t("info.items"),
            files_and_folders(tally.files, tally.folders),
        );
    }
    if let [item] = subject.items.as_slice()
        && let Some(modified) = &item.modified
    {
        row(
            t("label.column.modified"),
            format_timestamp(modified, today),
        );
    }
    // A folder on the phone (nothing selected): what the phone can do there.
    if subject.items.is_empty()
        && let Place::Phone {
            abilities: Some(abilities),
            ..
        } = &subject.place
    {
        let (can, cannot) = abilities_in_words(abilities);
        if !can.is_empty() {
            row(t("info.phone_can"), words_list(&can));
        }
        if !cannot.is_empty() {
            row(t("info.cant"), words_list(&cannot));
        }
    }

    let mut notes = Vec::new();
    if let Some(error) = error {
        notes.push(format!("{} {}", error.title, error.hint).trim().to_owned());
    }
    if counts_folders && error.is_none() {
        notes.push(t("info.note_hidden").to_owned());
    }
    if tally.unknown_sizes > 0 {
        notes.push(count("info.note_unknown", tally.unknown_sizes, &[]));
    }
    if tally.unreadable > 0 {
        notes.push(count("info.note_unreadable", tally.unreadable, &[]));
    }
    if tally.incomplete {
        notes.push(t("info.note_stopped").to_owned());
    }

    InfoResponse {
        title,
        rows,
        notes,
        kinds: kind_shares(tally),
    }
}

/// What fills the count, by kind, the most first: only for two or more
/// files with known sizes, and only kinds that hold something.
pub fn kind_shares(tally: &dto::Tally) -> Vec<super::response::KindShare> {
    if tally.files < 2 || tally.bytes == 0 {
        return Vec::new();
    }
    let mut shares: Vec<(dto::SpaceKind, u64)> = dto::SpaceKind::ALL
        .iter()
        .map(|&kind| (kind, tally.bytes_of(kind)))
        .filter(|(_, bytes)| *bytes > 0)
        .collect();
    shares.sort_by_key(|share| std::cmp::Reverse(share.1));
    shares
        .into_iter()
        .map(|(kind, bytes)| super::response::KindShare {
            kind,
            label: tf(
                "info.kind_share",
                &[
                    ("kind", kind_word(kind)),
                    ("size", &crate::handler::format::format_bytes(bytes)),
                ],
            ),
            portion: u16::try_from((u128::from(bytes) * 1000 / u128::from(tally.bytes)).max(1))
                .unwrap_or(1000),
            findable: kind.file_kind().is_some(),
        })
        .collect()
}

fn kind_word(kind: dto::SpaceKind) -> &'static str {
    match kind {
        dto::SpaceKind::Photos => t("label.kind.photos"),
        dto::SpaceKind::Videos => t("label.kind.videos"),
        dto::SpaceKind::Music => t("label.kind.music"),
        dto::SpaceKind::Documents => t("label.kind.documents"),
        dto::SpaceKind::Archives => t("label.kind.archives"),
        dto::SpaceKind::Apps => t("info.kind_apps"),
        dto::SpaceKind::Other => t("info.kind_other"),
    }
}

/// The name of the folder shown, or of the storage or disk at the top.
fn own_name(subject: &Subject) -> String {
    if subject.folder.trim_end_matches('/').is_empty() {
        return match &subject.place {
            Place::Phone { storage, .. } => storage.clone(),
            Place::Mac => english_only("Macintosh HD").to_owned(),
        };
    }
    explorer::folder_name(&subject.folder)
}

/// The folder the subject is in: the one shown for selected items, its parent
/// for the folder itself; nothing above the top.
fn where_it_is(subject: &Subject) -> Option<String> {
    let folder = if subject.items.is_empty() {
        let trimmed = subject.folder.trim_end_matches('/');
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.rsplit_once('/') {
            Some(("", _)) | None => "/".to_owned(),
            Some((parent, _)) => parent.to_owned(),
        }
    } else {
        subject.folder.clone()
    };
    Some(match &subject.place {
        Place::Phone { storage, .. } => {
            let path = folder.trim_matches('/');
            if path.is_empty() {
                tf("info.where_phone_top", &[("storage", storage)])
            } else {
                tf(
                    "info.where_phone",
                    &[("storage", storage), ("path", &path.replace('/', " › "))],
                )
            }
        }
        Place::Mac => folder,
    })
}

fn exact_bytes(bytes: u64) -> String {
    match bytes {
        0 => t("info.bytes_zero").to_owned(),
        _ if bytes < 1_000 => count("info.bytes", bytes as usize, &[]),
        _ => tf(
            "info.bytes_exact",
            &[
                ("size", &format_bytes(bytes)),
                (
                    "bytes",
                    &format_count(usize::try_from(bytes).unwrap_or(usize::MAX)),
                ),
            ],
        ),
    }
}

fn files_and_folders(files: usize, folders: usize) -> String {
    tf(
        "text.list_next",
        &[
            ("so_far", &count("text.files", files, &[])),
            ("next", &count("text.folders", folders, &[])),
        ],
    )
}

fn words_list(words: &[&str]) -> String {
    let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    comma_list(&words)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_phone_folder_says_what_the_phone_can_do() {
        let mut subject = phone("/DCIM", vec![]);
        if let Place::Phone { abilities, .. } = &mut subject.place {
            *abilities = Some(dto::Abilities {
                copy: true,
                moves: true,
                thumbnails: true,
                ..dto::Abilities::default()
            });
        }
        let today = NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        let rows = |subject: &Subject| {
            to_info_response(subject, &dto::Tally::default(), false, None, today).rows
        };

        let shown = rows(&subject);
        let value = |label: &str| {
            shown
                .iter()
                .find(|(name, _)| name == label)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            value("Phone can").as_deref(),
            Some("copy on the phone, move on the phone, make its own thumbnails")
        );
        assert!(
            value("Can't")
                .unwrap()
                .contains("resume a stopped download")
        );

        subject.items = vec![file("a.jpg")];
        assert!(
            rows(&subject).iter().all(|(name, _)| name != "Phone can"),
            "about the selection, not the phone"
        );
    }

    use super::*;
    use crate::handler::testing::{file, folder};

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 17).unwrap()
    }

    fn sized(name: &str, size_bytes: u64) -> dto::Entry {
        dto::Entry {
            size_bytes,
            ..file(name)
        }
    }

    fn phone(folder: &str, items: Vec<dto::Entry>) -> Subject {
        Subject {
            place: Place::Phone {
                storage_id: 5,
                storage: "Internal".into(),
                abilities: None,
            },
            folder: folder.into(),
            items,
        }
    }

    fn rows(response: &InfoResponse) -> Vec<(&str, &str)> {
        response
            .rows
            .iter()
            .map(|(label, value)| (label.as_str(), value.as_str()))
            .collect()
    }

    #[test]
    fn what_fills_a_count_is_split_by_kind_the_most_first() {
        let mut tally = dto::Tally::default();
        tally.add_file("clip.mp4", Some(41_200_000_000));
        tally.add_file("IMG_1.jpg", Some(12_800_000_000));
        tally.add_file("app.apk", Some(1_000_000));
        tally.add_file("odd.bin", Some(3_100_000_000));
        tally.add_file("film.mkv", None);
        let shares = kind_shares(&tally);
        let said: Vec<(&str, u16, bool)> = shares
            .iter()
            .map(|share| (share.label.as_str(), share.portion, share.findable))
            .collect();
        assert_eq!(
            said,
            [
                ("Videos 41.2 GB", 721, true),
                ("Photos 12.8 GB", 224, true),
                ("Other 3.1 GB", 54, false),
                ("Apps 1.0 MB", 1, false),
            ],
            "the unknown size is in none of them; a sliver still shows"
        );
        let mut one = dto::Tally::default();
        one.add_file("IMG_1.jpg", Some(5));
        assert!(kind_shares(&one).is_empty(), "a single file has no split");
        assert!(kind_shares(&dto::Tally::default()).is_empty());
    }

    #[test]
    fn the_selection_adds_its_files_and_its_folders_themselves() {
        let tally = base(&[
            sized("a.jpg", 10),
            sized("big.mp4", UNKNOWN_SIZE),
            folder("DCIM"),
        ]);

        assert_eq!(
            tally,
            dto::Tally {
                files: 2,
                folders: 1,
                bytes: 10,
                unknown_sizes: 1,
                by_kind: [10, 0, 0, 0, 0, 0, 0],
                ..dto::Tally::default()
            }
        );
    }

    #[test]
    fn only_folders_are_counted_and_the_folder_shown_when_nothing_is_selected() {
        let selected = phone(
            "/DCIM",
            vec![sized("a.jpg", 1), folder("Camera"), folder("Shots")],
        );
        assert_eq!(folders_to_count(&selected), ["/DCIM/Camera", "/DCIM/Shots"]);
        assert!(folders_to_count(&phone("/DCIM", vec![sized("a.jpg", 1)])).is_empty());
        assert_eq!(folders_to_count(&phone("/DCIM", vec![])), ["/DCIM"]);
    }

    #[test]
    fn one_file_says_its_kind_where_exact_size_and_date() {
        let mut photo = sized("IMG_0001.jpg", 3_402_112);
        photo.modified = file("x").modified;
        let subject = phone("/DCIM/Camera", vec![photo]);

        let response = to_info_response(&subject, &base(&subject.items), false, None, today());

        assert_eq!(response.title, "IMG_0001.jpg");
        assert_eq!(
            rows(&response)[..3],
            [
                ("Kind", "JPG file"),
                ("Where", "Internal › DCIM › Camera on the phone"),
                ("Size", "3.4 MB (3,402,112 bytes)"),
            ]
        );
        assert!(response.notes.is_empty(), "no folder was counted");
    }

    #[test]
    fn a_folder_says_counting_then_the_total_and_that_hidden_items_count() {
        let subject = phone("/DCIM", vec![folder("Camera")]);
        let mut tally = base(&subject.items);
        tally.add(&dto::Tally {
            files: 812,
            folders: 3,
            bytes: 2_000_000_000,
            ..dto::Tally::default()
        });

        let counting = to_info_response(&subject, &tally, true, None, today());
        assert!(rows(&counting).contains(&("Size", "Counting… 812 files, 2.0 GB so far")));

        let done = to_info_response(&subject, &tally, false, None, today());
        assert!(rows(&done).contains(&("Size", "2.0 GB (2,000,000,000 bytes)")));
        assert!(
            rows(&done).contains(&("Items", "812 files, 4 folders")),
            "{:?}",
            rows(&done)
        );
        assert_eq!(done.notes, ["Hidden items are counted too."]);
    }

    #[test]
    fn several_items_and_the_folder_itself_name_themselves() {
        let several = phone(
            "/",
            vec![sized("a.jpg", 1), folder("DCIM"), folder("Music")],
        );
        let response = to_info_response(&several, &base(&several.items), true, None, today());
        assert_eq!(response.title, "3 items");
        assert!(rows(&response).contains(&("Kind", "1 file, 2 folders")));
        assert!(rows(&response).contains(&("Where", "Internal on the phone")));

        let top = phone("/", vec![]);
        let response = to_info_response(&top, &dto::Tally::default(), true, None, today());
        assert_eq!(response.title, "Internal");
        assert!(
            !response.rows.iter().any(|(label, _)| label == "Where"),
            "nothing above the top"
        );

        let mac = Subject {
            place: Place::Mac,
            folder: "/Users/me/Pictures".into(),
            items: vec![],
        };
        let response = to_info_response(&mac, &dto::Tally::default(), false, None, today());
        assert_eq!(response.title, "Pictures");
        assert!(rows(&response).contains(&("Where", "/Users/me")));
        let disk = Subject {
            folder: "/".into(),
            ..mac
        };
        assert_eq!(
            to_info_response(&disk, &dto::Tally::default(), false, None, today()).title,
            "Macintosh HD"
        );
    }

    #[test]
    fn what_the_count_could_not_see_is_said() {
        let subject = phone("/", vec![]);
        let tally = dto::Tally {
            files: 3,
            unknown_sizes: 2,
            unreadable: 1,
            incomplete: true,
            ..dto::Tally::default()
        };

        let response = to_info_response(&subject, &tally, false, None, today());

        assert_eq!(response.notes.len(), 4, "{:?}", response.notes);
        assert!(response.notes[1].starts_with("2 files are 4 GB or more"));
        assert!(response.notes[2].starts_with("1 folder could not be read"));
        assert!(response.notes[3].contains("there is more"));

        let failed = to_info_response(
            &subject,
            &tally,
            false,
            Some(&ErrorResponse {
                title: "The phone stopped answering.".into(),
                hint: "Unplug it and plug it in again.".into(),
                device_lost: true,
                privacy: None,
                cancelled: false,
                source: dto::Error::Disconnected,
            }),
            today(),
        );
        assert!(rows(&failed).contains(&("Size", "Could not be counted")));
        assert!(failed.notes[0].starts_with("The phone stopped answering."));
    }

    #[test]
    fn exact_sizes_read_like_finders() {
        assert_eq!(exact_bytes(0), "Zero bytes");
        assert_eq!(exact_bytes(1), "1 byte");
        assert_eq!(exact_bytes(512), "512 bytes");
        assert_eq!(exact_bytes(1_234_567), "1.2 MB (1,234,567 bytes)");
    }
}
