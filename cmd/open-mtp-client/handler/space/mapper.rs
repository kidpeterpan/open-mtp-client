//! Free Up Space's words, and its list built once per look (never per redraw).

use chrono::NaiveDate;
use internal::dto;

use crate::handler::format::{format_bytes, format_count, format_timestamp};

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
        None => "Looking through the phone…".to_owned(),
        Some(dto::SpaceProgress::Walking { files }) => {
            format!("Looking through the phone: {} files", format_count(files))
        }
        Some(dto::SpaceProgress::Comparing { done, total }) => format!(
            "Reading files of the same size to be sure they are copies: {} of {}",
            format_bytes(done),
            format_bytes(total)
        ),
    }
}

/// What the look found, in a line.
pub fn found(report: &dto::SpaceReport) -> String {
    let copies: usize = report.duplicates.iter().map(|g| g.files.len() - 1).sum();
    let imported: u64 = report.imported.iter().map(|file| file.size).sum();
    match (copies, report.imported.len()) {
        (0, 0) => format!(
            "Nothing to free among {} files: no copies of the same file, and no photos already on this Mac.",
            format_count(report.files)
        ),
        (copies, photos) => {
            let mut parts = Vec::new();
            if copies > 0 {
                parts.push(format!(
                    "{} {} could go ({})",
                    format_count(copies),
                    if copies == 1 { "copy" } else { "copies" },
                    format_bytes(report.duplicate_bytes())
                ));
            }
            if photos > 0 {
                parts.push(format!(
                    "{} already on this Mac ({})",
                    files_word(photos),
                    format_bytes(imported)
                ));
            }
            parts.join(" · ")
        }
    }
}

/// The list: copies by group, the one to keep first, then the photos
/// already imported. Built once per look.
pub fn lines(report: &dto::SpaceReport, today: NaiveDate) -> Vec<Line> {
    let mut lines = vec![Line::Heading(format!(
        "Copies of the same file, read in full to be sure ({})",
        format_bytes(report.duplicate_bytes())
    ))];
    if report.duplicates.is_empty() {
        lines.push(Line::Empty("No two files are the same.".to_owned()));
    }
    for group in &report.duplicates {
        let name = group.files[0]
            .path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        lines.push(Line::Group(format!(
            "{name} · {} copies of {}",
            group.files.len(),
            format_bytes(group.size)
        )));
        for (index, file) in group.files.iter().enumerate() {
            lines.push(file_line(file, index == 0, today));
        }
    }
    let imported: u64 = report.imported.iter().map(|file| file.size).sum();
    lines.push(Line::Heading(format!(
        "Already imported and still on this Mac where the import put them ({})",
        format_bytes(imported)
    )));
    if report.imported.is_empty() {
        lines.push(Line::Empty(
            "None: only photos imported from now on, whose copy is still in place, count."
                .to_owned(),
        ));
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
pub fn remove_label(count: usize, bytes: u64) -> String {
    match count {
        0 => "Remove…".to_owned(),
        1 => format!("Remove 1 File ({})…", format_bytes(bytes)),
        n => format!(
            "Remove {} Files ({})…",
            format_count(n),
            format_bytes(bytes)
        ),
    }
}

/// The question before removing.
pub fn confirm(count: usize, bytes: u64) -> String {
    format!(
        "{} ({}) will be deleted from the phone for good. Each is looked up again first: one that changed since the look stays.",
        files_word(count),
        format_bytes(bytes)
    )
}

pub fn removing(done: usize, total: usize) -> String {
    format!(
        "Removing from the phone: {} of {}",
        format_count(done),
        files_word(total)
    )
}

/// How removing went: "Removed 5 files, 24 MB freed", and what stayed.
pub fn removed(removal: &dto::Removal) -> (String, Vec<String>) {
    let title = match removal.removed {
        0 => "Nothing was removed".to_owned(),
        n => format!(
            "Removed {}, {} freed",
            files_word(n),
            format_bytes(removal.freed)
        ),
    };
    let mut notes: Vec<String> = removal
        .kept
        .iter()
        .map(|kept| format!("{} stayed: {}", kept.label, why(&kept.why)))
        .collect();
    if let Some(stopped) = &removal.stopped {
        notes.push(format!(
            "Stopped: {}",
            crate::handler::error::to_error_response(stopped.clone()).title
        ));
    }
    (title, notes)
}

fn why(why: &dto::Why) -> String {
    match why {
        dto::Why::Changed => "it changed since the look, or wasn't in it".to_owned(),
        dto::Why::Refused(error) => crate::handler::error::to_error_response(error.clone()).title,
        _ => "the phone kept it".to_owned(),
    }
}

fn files_word(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{} files", format_count(n)),
    }
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
