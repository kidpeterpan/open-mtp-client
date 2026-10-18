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
