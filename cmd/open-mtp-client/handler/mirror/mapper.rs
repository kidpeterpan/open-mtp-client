use internal::dto;

use crate::handler::format::{format_bytes, format_count};
use crate::i18n::{count, key, t, tf};

fn files(number: usize) -> String {
    count("text.files", number, &[])
}

fn items(number: usize) -> String {
    count("text.items", number, &[])
}

pub fn explanation(direction: dto::MirrorDirection) -> &'static str {
    match direction {
        dto::MirrorDirection::ToMac => t("mirror.explain_to_mac"),
        dto::MirrorDirection::ToPhone => t("mirror.explain_to_phone"),
    }
}

pub fn hidden_note(direction: dto::MirrorDirection) -> &'static str {
    match direction {
        dto::MirrorDirection::ToMac => t("mirror.note_to_mac"),
        dto::MirrorDirection::ToPhone => t("mirror.note_to_phone"),
    }
}

/// What the dialog says while it compares.
pub fn comparing(seen: usize) -> String {
    match seen {
        0 => t("mirror.comparing").to_owned(),
        n => tf("mirror.comparing_so_far", &[("items", &items(n))]),
    }
}

/// The lines of a preview: what is copied, what is replaced, what is only in
/// the target and what becomes of it, and what is left alone.
pub fn preview_lines(
    preview: &dto::MirrorPreview,
    direction: dto::MirrorDirection,
    remove_extras: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    if preview.new_files + preview.changed_files == 0 && (preview.extras == 0 || !remove_extras) {
        lines.push(
            match direction {
                dto::MirrorDirection::ToMac => t("mirror.nothing_to_copy_mac"),
                dto::MirrorDirection::ToPhone => t("mirror.nothing_to_copy_phone"),
            }
            .to_owned(),
        );
    }
    if preview.new_files > 0 {
        lines.push(tf(
            "mirror.to_copy",
            &[
                ("files", &files(preview.new_files)),
                ("size", &format_bytes(preview.new_bytes)),
            ],
        ));
    }
    if preview.changed_files > 0 {
        lines.push(tf(
            "mirror.changed",
            &[
                ("files", &files(preview.changed_files)),
                ("size", &format_bytes(preview.changed_bytes)),
            ],
        ));
    }
    if preview.extras > 0 {
        let sentence = match (remove_extras, direction) {
            (false, dto::MirrorDirection::ToMac) => key("mirror.extras_kept_mac"),
            (false, dto::MirrorDirection::ToPhone) => key("mirror.extras_kept_phone"),
            (true, dto::MirrorDirection::ToMac) => key("mirror.extras_trash"),
            (true, dto::MirrorDirection::ToPhone) => key("mirror.extras_deleted"),
        };
        lines.push(tf(sentence, &[("items", &items(preview.extras))]));
    }
    if preview.conflicts > 0 {
        lines.push(tf(
            "mirror.conflicts",
            &[("items", &items(preview.conflicts))],
        ));
    }
    lines
}

/// A few of the paths of one kind: "a.jpg, b.jpg and 10 more".
pub fn sample_line(sample: &[String], total: usize) -> Option<String> {
    if sample.is_empty() {
        return None;
    }
    let shown = sample.join(", ");
    Some(match total.saturating_sub(sample.len()) {
        0 => shown,
        more => tf(
            "mirror.sample_more",
            &[("shown", &shown), ("more", &format_count(more))],
        ),
    })
}

/// A path as the user knows it: their home as "~".
pub fn mac_line(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') && !home.is_empty() => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(
        new_files: usize,
        changed_files: usize,
        extras: usize,
        conflicts: usize,
    ) -> dto::MirrorPreview {
        dto::MirrorPreview {
            new_files,
            new_bytes: new_files as u64 * 1_000_000,
            changed_files,
            changed_bytes: changed_files as u64 * 1_000_000,
            extras,
            conflicts,
            samples: dto::MirrorSamples::default(),
        }
    }

    #[test]
    fn the_preview_says_what_is_copied_replaced_and_what_becomes_of_the_rest() {
        let to_mac = dto::MirrorDirection::ToMac;
        let to_phone = dto::MirrorDirection::ToPhone;

        assert_eq!(
            preview_lines(&preview(12, 3, 4, 1), to_mac, false),
            [
                "12 files to copy (12.0 MB)",
                "3 files changed, to replace (3.0 MB)",
                "4 items only on the Mac: kept",
                "1 item left alone: names that clash, or a file where a folder is",
            ]
        );
        assert_eq!(
            preview_lines(&preview(0, 0, 4, 0), to_mac, true),
            ["4 items only on the Mac: to the Trash"]
        );
        assert_eq!(
            preview_lines(&preview(0, 0, 1, 0), to_phone, true),
            ["1 item only on the phone: deleted from the phone for good"]
        );
        assert_eq!(
            preview_lines(&preview(0, 0, 2, 0), to_phone, false),
            [
                "Nothing to copy: the phone has all of it already.",
                "2 items only on the phone: kept"
            ]
        );
    }

    #[test]
    fn a_sample_names_a_few_and_counts_the_rest() {
        let sample = vec!["a.jpg".to_owned(), "b.jpg".to_owned()];
        assert_eq!(sample_line(&sample, 2).as_deref(), Some("a.jpg, b.jpg"));
        assert_eq!(
            sample_line(&sample, 1_002).as_deref(),
            Some("a.jpg, b.jpg and 1,000 more")
        );
        assert_eq!(sample_line(&[], 0), None);
        assert_eq!(comparing(0), "Comparing the folders…");
        assert_eq!(
            comparing(1),
            "Comparing the folders… 1 item read on the phone"
        );
    }

    #[test]
    fn mac_paths_are_shown_from_home() {
        assert_eq!(mac_line("/Users/me/Backup", "/Users/me"), "~/Backup");
        assert_eq!(mac_line("/Users/me", "/Users/me"), "~");
        assert_eq!(mac_line("/Volumes/X", "/Users/me"), "/Volumes/X");
    }
}
