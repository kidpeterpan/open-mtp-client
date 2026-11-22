/// Which side a pinned folder is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSide {
    Mac,
    Phone,
}

/// The phone a pinned folder is on: by serial number (a USB port is not an
/// identity), and its storage by id and by name (an SD card put back can get
/// another id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinPhone {
    pub serial: String,
    pub storage_id: u64,
    pub storage_name: String,
}

/// A folder pinned to the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub side: PinSide,
    /// Absolute: a Mac path, or a phone path on its storage.
    pub path: String,
    pub label: String,
    /// For a phone folder.
    pub phone: Option<PinPhone>,
}

impl Pin {
    /// The same folder (the label does not count).
    pub fn same_place(&self, other: &Pin) -> bool {
        self.side == other.side
            && self.path == other.path
            && match (&self.phone, &other.phone) {
                (Some(a), Some(b)) => a.serial == b.serial && a.storage_id == b.storage_id,
                (None, None) => true,
                _ => false,
            }
    }
}

/// A mirror kept to run again: a folder on a phone's storage, a Mac folder,
/// which way, and whether what is only in the target goes too. Shown under
/// its phone (by serial) and never run by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMirror {
    pub phone: PinPhone,
    /// Absolute, on the phone's storage.
    pub phone_folder: String,
    /// Absolute, on the Mac.
    pub mac_folder: String,
    pub direction: super::MirrorDirection,
    pub remove_extras: bool,
}

impl SavedMirror {
    /// The same two folders on the same phone and storage, the same way
    /// (what it removes may differ: saving again changes that).
    pub fn same_mirror(&self, other: &SavedMirror) -> bool {
        self.phone.serial == other.phone.serial
            && self.phone.storage_id == other.phone.storage_id
            && self.phone_folder == other.phone_folder
            && self.mac_folder == other.mac_folder
            && self.direction == other.direction
    }

    pub fn label(&self) -> String {
        let phone = pin_label(&self.phone_folder, &self.phone.storage_name);
        let mac = pin_label(&self.mac_folder, "Mac");
        match self.direction {
            super::MirrorDirection::ToMac => format!("{phone} → {mac}"),
            super::MirrorDirection::ToPhone => format!("{mac} → {phone}"),
        }
    }
}

impl PinPhone {
    pub fn storage_among(&self, storages: &[(u64, String)]) -> Option<u64> {
        let by_id = storages.iter().find(|(id, _)| *id == self.storage_id);
        let by_name = || storages.iter().find(|(_, name)| *name == self.storage_name);
        by_id.or_else(by_name).map(|(id, _)| *id)
    }
}

pub fn pin_label(path: &str, top: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map_or_else(|| top.to_owned(), str::to_owned)
}

/// How many mirrors are kept.
pub const MOST_MIRRORS: usize = 20;

/// A Mac place in the sidebar: a folder of the user's, or a mounted disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub label: String,
    pub path: String,
    pub volume: bool,
    /// How full the disk is, for a disk whose mount macOS knows.
    pub space: Option<DiskSpace>,
}

/// A disk's room, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub free: u64,
    pub capacity: u64,
}

/// The most folders kept pinned.
pub const MOST_PINS: usize = 50;
/// The most mounted disks listed.
pub const MOST_VOLUMES: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::MirrorDirection;

    fn on_internal(phone_folder: &str, mac_folder: &str) -> SavedMirror {
        SavedMirror {
            phone: PinPhone {
                serial: "A".into(),
                storage_id: 1,
                storage_name: "Internal".into(),
            },
            phone_folder: phone_folder.into(),
            mac_folder: mac_folder.into(),
            direction: MirrorDirection::ToMac,
            remove_extras: false,
        }
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
        let pin = PinPhone {
            serial: "S".into(),
            storage_id: 2,
            storage_name: "SD card".into(),
        };
        let storages = vec![(1, "Internal".to_owned()), (3, "SD card".to_owned())];

        assert_eq!(
            pin.storage_among(&storages),
            Some(3),
            "by name: the id changed"
        );
        assert_eq!(
            pin.storage_among(&[(2, "Card".to_owned())]),
            Some(2),
            "by id first"
        );
        assert_eq!(pin.storage_among(&[(1, "Internal".to_owned())]), None);
    }

    #[test]
    fn a_saved_mirror_is_named_from_where_to_where() {
        assert_eq!(
            on_internal("/DCIM/Camera", "/Users/me/Phone photos").label(),
            "Camera → Phone photos"
        );
        let to_phone = SavedMirror {
            direction: MirrorDirection::ToPhone,
            ..on_internal("/", "/Users/me/Music")
        };
        assert_eq!(to_phone.label(), "Music → Internal");
        assert_eq!(on_internal("/DCIM", "/").label(), "DCIM → Mac");
    }
}
