use std::fs::Metadata;
use std::io;
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, Timelike};

use crate::dto::{Entry, Error, Timestamp};

pub fn to_entry(name: String, metadata: &Metadata) -> Entry {
    let is_folder = metadata.is_dir();
    Entry {
        id: 0,
        name,
        is_folder,
        size_bytes: if is_folder { 0 } else { metadata.len() },
        modified: metadata.modified().ok().map(to_timestamp),
    }
}

/// File times are instants; show them on the Mac's local clock.
pub fn to_timestamp(time: SystemTime) -> Timestamp {
    let local: DateTime<Local> = time.into();
    Timestamp {
        year: u16::try_from(local.year()).unwrap_or_default(),
        month: local.month() as u8,
        day: local.day() as u8,
        hour: local.hour() as u8,
        minute: local.minute() as u8,
    }
}

pub fn to_error(err: &io::Error, path: &str) -> Error {
    match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
            Error::PathNotFound(path.to_owned())
        }
        io::ErrorKind::PermissionDenied => Error::FolderAccessDenied(path.to_owned()),
        io::ErrorKind::StorageFull => Error::MacDiskFull,
        io::ErrorKind::ReadOnlyFilesystem => Error::VolumeReadOnly(format!("in “{path}”")),
        io::ErrorKind::FileTooLarge => Error::TooBigForVolume(format!("in “{path}”")),
        io::ErrorKind::AlreadyExists => {
            Error::NameTaken(path.rsplit('/').next().unwrap_or(path).to_owned())
        }
        _ => Error::General(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use chrono::TimeZone;

    use super::*;

    #[test]
    fn a_read_only_disk_and_a_file_too_big_for_it_are_named_as_such() {
        let read_only = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
        assert_eq!(
            to_error(&read_only, "/Volumes/BACKUP/a.jpg"),
            Error::VolumeReadOnly("in “/Volumes/BACKUP/a.jpg”".into())
        );
        let too_big = io::Error::from(io::ErrorKind::FileTooLarge);
        assert_eq!(
            to_error(&too_big, "/Volumes/KINGSTON/v.mp4"),
            Error::TooBigForVolume("in “/Volumes/KINGSTON/v.mp4”".into())
        );
        assert_eq!(
            to_error(&io::Error::from_raw_os_error(libc::EROFS), "/x"),
            Error::VolumeReadOnly("in “/x”".into()),
            "the code macOS gives"
        );
        assert_eq!(
            to_error(&io::Error::from_raw_os_error(libc::EFBIG), "/x"),
            Error::TooBigForVolume("in “/x”".into())
        );
    }

    #[test]
    fn timestamps_use_the_local_clock() {
        let instant = 1_791_376_200;
        let local = Local.timestamp_opt(instant, 0).unwrap();

        let stamp = to_timestamp(UNIX_EPOCH + Duration::from_secs(instant as u64));

        assert_eq!(u32::from(stamp.hour), local.hour());
        assert_eq!(u32::from(stamp.day), local.day());
    }

    #[test]
    fn io_errors_map_to_what_the_user_can_act_on() {
        let not_found = io::Error::from(io::ErrorKind::NotFound);
        let not_dir = io::Error::from(io::ErrorKind::NotADirectory);
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let other = io::Error::other("disk on fire");
        let exists = io::Error::from(io::ErrorKind::AlreadyExists);
        let full = io::Error::from(io::ErrorKind::StorageFull);

        assert_eq!(to_error(&not_found, "/x"), Error::PathNotFound("/x".into()));
        assert_eq!(to_error(&not_dir, "/x"), Error::PathNotFound("/x".into()));
        assert_eq!(
            to_error(&denied, "/x"),
            Error::FolderAccessDenied("/x".into())
        );
        assert_eq!(
            to_error(&other, "/x"),
            Error::General("disk on fire".into())
        );
        assert_eq!(
            to_error(&exists, "/a/Trip"),
            Error::NameTaken("Trip".into())
        );
        assert_eq!(to_error(&full, "/a"), Error::MacDiskFull);
    }
}
