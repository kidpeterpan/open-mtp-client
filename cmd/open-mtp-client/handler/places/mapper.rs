use internal::dto;

use crate::i18n::{t, tf};
use crate::icons;

pub fn place_label(place: &dto::Place) -> String {
    if place.volume {
        return place.label.clone();
    }
    let folder = place.path.trim_end_matches('/').rsplit('/').next();
    let label = match folder {
        Some("Desktop") => t("places.desktop"),
        Some("Downloads") => t("places.downloads"),
        Some("Pictures") => t("places.pictures"),
        _ => t("places.home"),
    };
    label.to_owned()
}

/// The icon beside a place: a disk, the home folder, or a usual folder of it.
pub fn place_symbol(place: &dto::Place) -> char {
    if place.volume {
        return icons::HARD_DRIVE;
    }
    match icons::folder_at(&place.path) {
        icons::FOLDER => icons::HOUSE,
        usual => usual,
    }
}

/// A disk's room as the sidebar shows it: how much of the bar is used, and
/// "312.0 GB free of 1.0 TB", as the phone's storages say it.
pub fn disk_usage(space: &dto::DiskSpace) -> (f32, String) {
    let used = if space.capacity == 0 {
        0.0
    } else {
        (space.capacity.saturating_sub(space.free) as f64 / space.capacity as f64) as f32
    };
    (
        used.clamp(0.0, 1.0),
        tf(
            "places.free_of",
            &[
                ("free", &crate::handler::format::format_bytes(space.free)),
                (
                    "total",
                    &crate::handler::format::format_bytes(space.capacity),
                ),
            ],
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disks_room_reads_as_the_phones_does() {
        let (used, words) = disk_usage(&dto::DiskSpace {
            free: 312_000_000_000,
            capacity: 1_000_000_000_000,
        });
        assert!((used - 0.688).abs() < 0.001, "{used}");
        assert_eq!(words, "312.0 GB free of 1.0 TB");
        let (used, _) = disk_usage(&dto::DiskSpace {
            free: 0,
            capacity: 0,
        });
        assert_eq!(used, 0.0, "a disk that says nothing is not full");
    }

    fn place(path: &str, volume: bool) -> dto::Place {
        dto::Place {
            label: "Macintosh HD".into(),
            path: path.into(),
            volume,
            space: None,
        }
    }

    #[test]
    fn a_place_gets_the_icon_of_what_it_is() {
        assert_eq!(place_symbol(&place("/", true)), icons::HARD_DRIVE);
        assert_eq!(place_symbol(&place("/Users/me", false)), icons::HOUSE);
        assert_eq!(
            place_symbol(&place("/Users/me/Desktop", false)),
            icons::MONITOR
        );
        assert_eq!(
            place_symbol(&place("/Users/me/Downloads", false)),
            icons::DOWNLOAD
        );
        assert_eq!(
            place_symbol(&place("/Users/me/Pictures", false)),
            icons::IMAGE
        );
    }
}
