use internal::dto;

/// A pinned folder's label: its name, or for the top of a storage or of the
/// Mac, the storage's or the disk's name.
pub fn pin_label(path: &str, top: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map_or_else(|| top.to_owned(), str::to_owned)
}

/// The storage a phone pin is on, among the open phone's: by id, else by
/// name (an SD card put back can get another id); `None` if it isn't there.
pub fn storage_of(pin: &dto::PinPhone, storages: &[(u64, String)]) -> Option<u64> {
    storages
        .iter()
        .find(|(id, _)| *id == pin.storage_id)
        .or_else(|| storages.iter().find(|(_, name)| *name == pin.storage_name))
        .map(|(id, _)| *id)
}

/// A saved mirror as the sidebar names it: the folder copied from, an
/// arrow, and the folder it keeps like it ("Camera → Phone photos").
pub fn mirror_label(mirror: &dto::SavedMirror) -> String {
    let phone = pin_label(&mirror.phone_folder, &mirror.phone.storage_name);
    let mac = pin_label(&mirror.mac_folder, "Mac");
    match mirror.direction {
        dto::MirrorDirection::ToMac => format!("{phone} → {mac}"),
        dto::MirrorDirection::ToPhone => format!("{mac} → {phone}"),
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
        format!(
            "{} free of {}",
            crate::handler::format::format_bytes(space.free),
            crate::handler::format::format_bytes(space.capacity)
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

    #[test]
    fn a_pin_is_called_by_its_folders_name_or_the_storages_at_the_top() {
        assert_eq!(pin_label("/DCIM/Camera", "Internal"), "Camera");
        assert_eq!(pin_label("/DCIM/Camera/", "Internal"), "Camera");
        assert_eq!(pin_label("/", "Internal"), "Internal");
        assert_eq!(pin_label("/", "Macintosh HD"), "Macintosh HD");
    }

    #[test]
    fn a_pins_storage_is_found_by_id_then_by_name() {
        let pin = dto::PinPhone {
            serial: "S".into(),
            storage_id: 2,
            storage_name: "SD card".into(),
        };
        let storages = vec![(1, "Internal".to_owned()), (3, "SD card".to_owned())];

        assert_eq!(
            storage_of(&pin, &storages),
            Some(3),
            "by name: the id changed"
        );
        assert_eq!(
            storage_of(&pin, &[(2, "Card".to_owned())]),
            Some(2),
            "by id first"
        );
        assert_eq!(storage_of(&pin, &[(1, "Internal".to_owned())]), None);
    }
}
