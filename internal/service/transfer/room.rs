//! Room before a byte moves. A transfer's plan says how much it will write;
//! before the first byte it is compared with the free space where it is
//! going, and one that can't fit is refused at once with both numbers,
//! rather than starting and stopping part way without saying why.

use std::path::Path;
use std::sync::Arc;

use crate::dto::Error;

/// How much room there is somewhere, and how to name that place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub free: u64,
    /// "on your Mac's disk", "on “USB STICK”", "on the phone's “Internal
    /// shared storage”".
    pub place: String,
    /// Nothing can be written there (an NTFS drive, a locked card).
    pub read_only: bool,
    /// The biggest file its file system holds, where that is a limit
    /// (FAT32: 4 GB less a byte); `None` for no practical limit.
    pub most_per_file: Option<u64>,
}

impl Room {
    /// Room with no limit but its free space.
    pub fn free(free: u64, place: impl Into<String>) -> Self {
        Self {
            free,
            place: place.into(),
            read_only: false,
            most_per_file: None,
        }
    }
}

/// The biggest file FAT32 holds.
const FAT32_MOST: u64 = 0xFFFF_FFFF;

/// `statfs`'s flag for a volume mounted read-only (`sys/mount.h`).
const MNT_RDONLY: u32 = 0x0000_0001;

/// Refuses a write anywhere on a read-only room before a byte moves.
pub(super) fn ensure_writable(room: Option<&Room>) -> Result<(), Error> {
    match room {
        Some(room) if room.read_only => Err(Error::VolumeReadOnly(room.place.clone())),
        _ => Ok(()),
    }
}

/// Whether a file of `size` bytes (as the phone reports it) is too big for
/// `room`'s file system: past its limit, or MTP's "4 GB or more", which a
/// FAT32 disk can't take either.
pub(super) fn too_big(size: u64, room: Option<&Room>) -> bool {
    room.and_then(|room| room.most_per_file)
        .is_some_and(|most| size > most || (most <= FAT32_MOST && size == FAT32_MOST))
}

/// Where the free space of a Mac folder comes from: the real disk in the
/// app, a fixed answer in tests.
pub(super) type RoomOf = Arc<dyn Fn(&Path) -> std::io::Result<Room> + Send + Sync>;

/// Refuses `needed` bytes when `room` has less free; a room that can't tell
/// (none, or MTP's `u64::MAX`, which nothing exceeds) lets it go ahead.
pub(super) fn ensure(needed: u64, room: Option<&Room>) -> Result<(), Error> {
    match room {
        Some(room) if needed > room.free => Err(Error::NotEnoughRoom {
            needed,
            free: room.free,
            place: room.place.clone(),
        }),
        _ => Ok(()),
    }
}

/// A phone storage's room, from what it reported when it was last fetched.
pub(super) fn phone_room(free: u64, name: &str) -> Room {
    Room::free(free, format!("on the phone's “{name}”"))
}

/// The free space of the volume `path` is on (the nearest folder above it
/// that exists), as the user may use it, and that volume's name.
pub fn mac_room(path: &Path) -> std::io::Result<Room> {
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .unwrap_or(Path::new("/"));
    let c_path = std::ffi::CString::new(existing.as_os_str().as_encoded_bytes())
        .map_err(std::io::Error::other)?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid C string and `stats` is a zeroed `statfs`
    // the call fills in.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `f_mntonname` and `f_fstypename` are NUL-terminated arrays
    // statfs filled in.
    let (mounted, kind) = unsafe {
        (
            std::ffi::CStr::from_ptr(stats.f_mntonname.as_ptr()),
            std::ffi::CStr::from_ptr(stats.f_fstypename.as_ptr()),
        )
    };
    Ok(room_of(
        stats.f_bavail.saturating_mul(u64::from(stats.f_bsize)),
        &mounted.to_string_lossy(),
        &kind.to_string_lossy(),
        stats.f_flags,
    ))
}

/// The room `statfs` describes: its free bytes, where it is mounted, its
/// file system's name (`apfs`, `msdos`, `exfat`, `ntfs`…) and its flags.
fn room_of(free: u64, mounted: &str, kind: &str, flags: u32) -> Room {
    Room {
        read_only: flags & MNT_RDONLY != 0,
        most_per_file: (kind == "msdos").then_some(FAT32_MOST),
        ..Room::free(free, place_of(mounted))
    }
}

/// How a volume mounted at `mounted` is named to the user.
fn place_of(mounted: &str) -> String {
    match Path::new(mounted).file_name() {
        Some(name) if mounted != "/" => format!("on “{}”", name.to_string_lossy()),
        _ => "on your Mac's disk".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(free: u64) -> Room {
        Room::free(free, "on your Mac's disk")
    }

    #[test]
    fn a_volume_says_whether_it_can_be_written_and_how_big_a_file_it_holds() {
        let apfs = room_of(9, "/", "apfs", 0x0000_1000);
        assert_eq!(apfs, Room::free(9, "on your Mac's disk"));
        let ntfs = room_of(9, "/Volumes/BACKUP", "ntfs", MNT_RDONLY | 0x0000_1000);
        assert!(ntfs.read_only && ntfs.most_per_file.is_none());
        let fat = room_of(9, "/Volumes/KINGSTON", "msdos", 0);
        assert_eq!(
            (fat.read_only, fat.most_per_file, fat.place.as_str()),
            (false, Some(FAT32_MOST), "on “KINGSTON”")
        );
        assert_eq!(room_of(9, "/Volumes/X", "exfat", 0).most_per_file, None);

        assert_eq!(
            ensure_writable(Some(&ntfs)),
            Err(Error::VolumeReadOnly("on “BACKUP”".into()))
        );
        assert_eq!(ensure_writable(Some(&fat)), Ok(()));
        assert_eq!(ensure_writable(None), Ok(()));
    }

    #[test]
    fn four_gigabytes_or_more_is_too_big_for_fat32_and_nothing_else() {
        let fat = room_of(9, "/Volumes/KINGSTON", "msdos", 0);
        assert!(!too_big(4_294_967_294, Some(&fat)));
        assert!(too_big(4_294_967_296, Some(&fat)));
        assert!(too_big(5_000_000_000, Some(&fat)));
        assert!(too_big(0xFFFF_FFFF, Some(&fat)), "MTP's “4 GB or more”");
        assert!(!too_big(5_000_000_000, Some(&room(9))));
        assert!(!too_big(5_000_000_000, None));
    }

    #[test]
    fn a_transfer_that_cannot_fit_is_refused_with_both_numbers() {
        assert_eq!(
            ensure(100, Some(&room(40))),
            Err(Error::NotEnoughRoom {
                needed: 100,
                free: 40,
                place: "on your Mac's disk".into(),
            })
        );
        assert_eq!(ensure(40, Some(&room(40))), Ok(()), "exactly enough");
        assert_eq!(ensure(100, None), Ok(()), "no word on the room: go ahead");
        assert_eq!(
            ensure(100, Some(&room(u64::MAX))),
            Ok(()),
            "the phone can't tell"
        );
        assert_eq!(ensure(0, Some(&room(0))), Ok(()), "nothing to write");
    }

    #[test]
    fn volumes_are_named_as_finder_names_them() {
        assert_eq!(place_of("/"), "on your Mac's disk");
        assert_eq!(place_of("/Volumes/USB STICK"), "on “USB STICK”");
        assert_eq!(
            phone_room(5, "Internal shared storage").place,
            "on the phone's “Internal shared storage”"
        );
    }

    // The real disk: a query, nothing changes.
    #[test]
    fn the_macs_own_disk_says_how_much_is_free() {
        let dir = tempfile::tempdir().unwrap();

        let here = mac_room(dir.path()).unwrap();
        let below = mac_room(&dir.path().join("not/made/yet")).unwrap();

        assert!(here.free > 0);
        assert_eq!(below.place, here.place, "the nearest folder that exists");
        assert!(!here.read_only, "a temporary folder can be written");
        assert_eq!(here.most_per_file, None, "this Mac's own disk is APFS");
    }
}
