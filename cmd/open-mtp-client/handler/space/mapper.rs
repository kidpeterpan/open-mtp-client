//! Free Up Space's words, and its list built once per look (never per redraw).

use chrono::NaiveDate;
use internal::dto;

use crate::handler::format::{format_bytes, format_count, format_timestamp};
use crate::i18n::{count, t, tf};

/// One line of the dialog's list, all the same height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A section's title: "Copies of the same file".
    Heading(String),
    /// A group's head: "IMG_1.jpg · 3 copies of 300 KB".
    Group(String),
    /// A file that can be picked.
    File {
        path: String,
        /// "300 KB · 12 Oct 2026".
        detail: String,
        /// The copy to keep: marked, and left out of "Select Copies".
        original: bool,
    },
    /// Said when a section has nothing in it.
    Empty(String),
}

pub fn looking(progress: Option<dto::SpaceProgress>) -> String {
    match progress {
        None => t("space.looking").to_owned(),
        Some(dto::SpaceProgress::Walking { files }) => {
            tf("space.walking", &[("files", &files_word(files))])
        }
        Some(dto::SpaceProgress::Comparing { done, total }) => tf(
            "space.comparing",
            &[
                ("done", &format_bytes(done)),
                ("total", &format_bytes(total)),
            ],
        ),
    }
}

/// What the look found, in a line.
pub fn found(report: &dto::SpaceReport) -> String {
    let copies: usize = report.duplicates.iter().map(|g| g.files.len() - 1).sum();
    let imported: u64 = report.imported.iter().map(|file| file.size).sum();
    match (copies, report.imported.len()) {
        (0, 0) => tf(
            "space.nothing_to_free",
            &[("files", &files_word(report.files))],
        ),
        (copies, photos) => {
            let mut parts = Vec::new();
            if copies > 0 {
                parts.push(count(
                    "space.copies_could_go",
                    copies,
                    &[("size", &format_bytes(report.duplicate_bytes()))],
                ));
            }
            if photos > 0 {
                parts.push(tf(
                    "space.imported_already",
                    &[
                        ("files", &files_word(photos)),
                        ("size", &format_bytes(imported)),
                    ],
                ));
            }
            parts.join(" · ")
        }
    }
}

/// The list: copies by group, the one to keep first, then the photos
/// already imported. Built once per look.
pub fn lines(report: &dto::SpaceReport, today: NaiveDate) -> Vec<Line> {
    let mut lines = vec![Line::Heading(tf(
        "space.heading_copies",
        &[("size", &format_bytes(report.duplicate_bytes()))],
    ))];
    if report.duplicates.is_empty() {
        lines.push(Line::Empty(t("space.no_two_same").to_owned()));
    }
    for group in &report.duplicates {
        let name = group.files[0]
            .path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        lines.push(Line::Group(count(
            "space.group",
            group.files.len(),
            &[("name", &name), ("size", &format_bytes(group.size))],
        )));
        for (index, file) in group.files.iter().enumerate() {
            lines.push(file_line(file, index == 0, today));
        }
    }
    let imported: u64 = report.imported.iter().map(|file| file.size).sum();
    lines.push(Line::Heading(tf(
        "space.heading_imported",
        &[("size", &format_bytes(imported))],
    )));
    if report.imported.is_empty() {
        lines.push(Line::Empty(t("space.none_imported").to_owned()));
    }
    for file in &report.imported {
        lines.push(file_line(file, false, today));
    }
    lines
}

fn file_line(file: &dto::SpaceFile, original: bool, today: NaiveDate) -> Line {
    let mut detail = format_bytes(file.size);
    if let Some(when) = &file.modified {
        detail.push_str(&format!(" · {}", format_timestamp(when, today)));
    }
    Line::File {
        path: file.path.clone(),
        detail,
        original,
    }
}

/// The button that asks before removing: "Remove 5 Files (24 MB)…".
pub fn remove_label(number: usize, bytes: u64) -> String {
    match number {
        0 => t("space.remove").to_owned(),
        n => count("space.remove_files", n, &[("size", &format_bytes(bytes))]),
    }
}

/// The question before removing.
pub fn confirm(number: usize, bytes: u64) -> String {
    tf(
        "space.confirm",
        &[
            ("files", &files_word(number)),
            ("size", &format_bytes(bytes)),
        ],
    )
}

pub fn removing(done: usize, total: usize) -> String {
    tf(
        "space.removing",
        &[("done", &format_count(done)), ("files", &files_word(total))],
    )
}

/// How removing went: "Removed 5 files, 24 MB freed", and what stayed.
pub fn removed(removal: &dto::Removal) -> (String, Vec<String>) {
    let title = match removal.removed {
        0 => t("space.nothing_removed").to_owned(),
        n => tf(
            "space.removed",
            &[
                ("files", &files_word(n)),
                ("size", &format_bytes(removal.freed)),
            ],
        ),
    };
    let mut notes: Vec<String> = removal
        .kept
        .iter()
        .map(|kept| {
            tf(
                "space.stayed",
                &[("label", &kept.label), ("why", &why(&kept.why))],
            )
        })
        .collect();
    if let Some(stopped) = &removal.stopped {
        notes.push(tf(
            "space.stopped",
            &[("why", &crate::handler::error::describe_again(stopped).title)],
        ));
    }
    (title, notes)
}

fn why(why: &dto::Why) -> String {
    match why {
        dto::Why::Changed => t("space.why_changed").to_owned(),
        dto::Why::Refused(error) => crate::handler::error::describe_again(error).title,
        _ => t("space.why_kept").to_owned(),
    }
}

fn files_word(number: usize) -> String {
    count("text.files", number, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 18).unwrap()
    }

    #[test]
    fn a_phone_with_nothing_to_free_says_so_and_both_sections_say_why() {
        let report = dto::SpaceReport {
            files: 1_200,
            ..Default::default()
        };
        assert_eq!(
            found(&report),
            "Nothing to free among 1,200 files: no copies of the same file, and no photos already on this Mac."
        );
        let lines = lines(&report, today());
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1], Line::Empty("No two files are the same.".into()));
        assert!(
            matches!(&lines[3], Line::Empty(said) if said.starts_with("None: only photos imported from now on"))
        );
    }

    #[test]
    fn one_copy_is_one_and_a_files_line_has_its_size_and_date() {
        let file = |path: &str| dto::SpaceFile {
            path: path.into(),
            size: 1_000,
            modified: Some(dto::Timestamp {
                year: 2026,
                month: 10,
                day: 18,
                hour: 9,
                minute: 30,
            }),
        };
        let report = dto::SpaceReport {
            files: 2,
            duplicates: vec![dto::DuplicateGroup {
                size: 1_000,
                files: vec![file("/a.jpg"), file("/b.jpg")],
            }],
            ..Default::default()
        };
        assert_eq!(found(&report), "1 copy could go (1.0 KB)");
        match &lines(&report, today())[2] {
            Line::File {
                path,
                detail,
                original,
            } => {
                assert_eq!((path.as_str(), *original), ("/a.jpg", true));
                assert!(detail.starts_with("1.0 KB · "), "{detail}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn what_removing_did_is_said_with_what_stayed_and_why_it_stopped() {
        assert_eq!(remove_label(1, 1_000), "Remove 1 File (1.0 KB)…");
        assert_eq!(removing(2, 5), "Removing from the phone: 2 of 5 files");
        let (title, notes) = removed(&dto::Removal {
            kept: vec![dto::Kept {
                label: "/a.jpg".into(),
                why: dto::Why::Refused(dto::Error::PermissionDenied),
            }],
            stopped: Some(dto::Error::Cancelled),
            ..Default::default()
        });
        assert_eq!(title, "Nothing was removed");
        assert_eq!(notes.len(), 2);
        assert!(notes[0].starts_with("/a.jpg stayed: "), "{notes:?}");
        assert!(notes[1].starts_with("Stopped: "), "{notes:?}");
    }
}
