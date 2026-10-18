use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mtp_rs::mtp::{DateTime, UtcOffset};

use crate::service::{clock, names};

/// A device date as an instant, for stamping the copy. Zoneless dates (what
/// Android writes) are read in the Mac's zone, like the listing shows them.
pub fn to_system_time(time: DateTime) -> Option<SystemTime> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let zone = UtcOffset::from_minutes(clock::local_offset_minutes(now))?;
    let secs = time.to_unix_seconds_with_fallback(zone)?;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
}

/// Whether two file times are the same moment as far as copies can tell. FAT
/// file systems keep only every other second, so a copy's date may be up to
/// two seconds off the original's.
pub fn same_moment(a: SystemTime, b: SystemTime) -> bool {
    let apart = match a.duration_since(b) {
        Ok(apart) => apart,
        Err(err) => err.duration(),
    };
    apart <= Duration::from_secs(2)
}

/// A Mac file time as a device date, written with the Mac's offset so the
/// phone stores the same instant.
pub fn to_device_time(time: SystemTime) -> Option<DateTime> {
    let secs = i64::try_from(time.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    let zone = UtcOffset::from_minutes(clock::local_offset_minutes(secs))?;
    DateTime::from_unix_seconds(secs, zone)
}

/// Finder's "keep both" naming: `IMG_001 (2).jpg`, `DCIM (2)`. `n` 0 is the
/// name itself.
pub fn numbered(name: &str, n: usize, is_folder: bool) -> String {
    names::numbered(name, n, is_folder)
}

/// Hidden sibling a file is written to before it is complete; `n` picks
/// another one when that name is taken.
pub fn part_name(name: &str, n: usize) -> String {
    match n {
        0 => format!(".{name}.open-mtp-client-part"),
        n => format!(".{name}.{n}.open-mtp-client-part"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_keeps_the_extension_of_files() {
        assert_eq!(numbered("IMG_001.jpg", 0, false), "IMG_001.jpg");
        assert_eq!(numbered("IMG_001.jpg", 1, false), "IMG_001 (1).jpg");
        assert_eq!(numbered("archive.tar.gz", 2, false), "archive.tar (2).gz");
        assert_eq!(numbered("README", 1, false), "README (1)");
        assert_eq!(numbered(".nomedia", 1, false), ".nomedia (1)");
    }

    #[test]
    fn numbered_folders_ignore_dots() {
        assert_eq!(numbered("v1.2", 1, true), "v1.2 (1)");
    }

    #[test]
    fn part_files_are_hidden_siblings() {
        assert_eq!(part_name("a.jpg", 0), ".a.jpg.open-mtp-client-part");
        assert_eq!(part_name("a.jpg", 2), ".a.jpg.2.open-mtp-client-part");
    }

    #[test]
    fn mac_times_round_trip_through_device_dates() {
        let instant = UNIX_EPOCH + Duration::from_secs(1_791_376_200);

        let device = to_device_time(instant).unwrap();

        assert_eq!(to_system_time(device), Some(instant));
    }

    #[test]
    fn device_dates_become_instants() {
        let time = DateTime {
            year: 2026,
            month: 10,
            day: 7,
            hour: 13,
            minute: 50,
            second: 0,
            offset: Some(UtcOffset::UTC),
        };

        let instant = to_system_time(time).unwrap();

        assert_eq!(
            instant.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_791_381_000
        );
    }

    #[test]
    fn times_within_two_seconds_are_the_same_moment() {
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);

        assert!(same_moment(at(1_000), at(1_000)));
        assert!(same_moment(at(1_000), at(1_002)), "FAT keeps even seconds");
        assert!(same_moment(at(1_002), at(1_000)), "either way round");
        assert!(!same_moment(at(1_000), at(1_003)));
        assert!(!same_moment(at(1_003), at(1_000)));
        assert!(!same_moment(at(1_000), at(2_000)));
    }
}
