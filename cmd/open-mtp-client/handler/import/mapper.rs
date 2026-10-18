use internal::dto;

use crate::handler::format::{format_bytes, format_count};

/// "1 photo or video", "312 photos and videos".
fn photos(count: usize) -> String {
    match count {
        1 => "1 photo or video".to_owned(),
        n => format!("{} photos and videos", format_count(n)),
    }
}

/// What the dialog says while it looks.
pub fn looking(seen: usize) -> String {
    match seen {
        0 => "Looking through DCIM…".to_owned(),
        n => format!("Looking through DCIM… {} so far", photos(n)),
    }
}

/// What the dialog says about what is new.
pub fn preview_line(preview: &dto::ImportPreview) -> String {
    match (preview.new_files, preview.already) {
        (0, 0) => "There are no photos or videos in DCIM.".to_owned(),
        (0, already) => format!(
            "Nothing new: all {} are on this Mac already.",
            photos(already)
        ),
        (new, 0) => format!(
            "{} to import ({})",
            photos(new),
            format_bytes(preview.new_bytes)
        ),
        (new, already) => format!(
            "{} to import ({}) · {} already on this Mac",
            photos(new),
            format_bytes(preview.new_bytes),
            format_count(already)
        ),
    }
}

/// What the dialog says about a storage without a DCIM folder: not an error
/// so much as nothing from a camera to import.
pub fn no_dcim(storage: &str) -> String {
    format!("{storage} has no DCIM folder, so there is nothing from the camera to import.")
}

/// The destination as the user knows it: their home as "~".
pub fn destination_line(destination: &str, home: &str) -> String {
    match destination.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') && !home.is_empty() => format!("~{rest}"),
        _ => destination.to_owned(),
    }
}

/// What the dialog says under "Remove them from the phone".
pub fn removing_note(remove: bool) -> &'static str {
    if remove {
        "Only photos and videos whose copies are checked on the Mac are removed, one by one; folders stay."
    } else {
        "Everything stays on the phone."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(new_files: usize, new_bytes: u64, already: usize) -> dto::ImportPreview {
        dto::ImportPreview {
            new_files,
            new_bytes,
            already,
        }
    }

    #[test]
    fn the_preview_says_what_is_new_and_what_is_there() {
        assert_eq!(
            preview_line(&preview(312, 4_100_000_000, 4_898)),
            "312 photos and videos to import (4.1 GB) · 4,898 already on this Mac"
        );
        assert_eq!(
            preview_line(&preview(1, 2_000_000, 0)),
            "1 photo or video to import (2.0 MB)"
        );
        assert_eq!(
            preview_line(&preview(0, 0, 4_898)),
            "Nothing new: all 4,898 photos and videos are on this Mac already."
        );
        assert_eq!(
            preview_line(&preview(0, 0, 0)),
            "There are no photos or videos in DCIM."
        );
    }

    #[test]
    fn looking_counts_as_it_goes() {
        assert_eq!(looking(0), "Looking through DCIM…");
        assert_eq!(looking(1), "Looking through DCIM… 1 photo or video so far");
        assert_eq!(
            looking(1_200),
            "Looking through DCIM… 1,200 photos and videos so far"
        );
    }

    #[test]
    fn a_storage_without_dcim_has_nothing_to_import() {
        assert_eq!(
            no_dcim("SD card"),
            "SD card has no DCIM folder, so there is nothing from the camera to import."
        );
    }

    #[test]
    fn the_destination_is_shown_from_home() {
        assert_eq!(
            destination_line("/Users/me/Pictures", "/Users/me"),
            "~/Pictures"
        );
        assert_eq!(destination_line("/Users/me", "/Users/me"), "~");
        assert_eq!(
            destination_line("/Users/meg/x", "/Users/me"),
            "/Users/meg/x"
        );
        assert_eq!(
            destination_line("/Volumes/Photos", "/Users/me"),
            "/Volumes/Photos"
        );
    }
}
