//! Error copy shared by every handler: a short title plus one concrete next step.
//!
//! Every error shown is also noted, by its title alone, in a short history of
//! recent problems that the diagnostics report includes. A title is fixed
//! text (`&'static str`): never a path, a file name or a serial, which only
//! the hints carry. The history is kept in memory and is gone when the app
//! quits.

use std::collections::VecDeque;

use chrono::{DateTime, Local};
use internal::dto;

use crate::i18n::{english, key, t, tf};

#[derive(Debug, Clone)]
pub struct ErrorResponse {
    pub title: String,
    pub hint: String,
    /// The session is gone; the device panel should offer to reconnect.
    pub device_lost: bool,
    /// macOS refused a folder: the System Settings pane that lets the app in.
    pub privacy: Option<dto::PrivacyPane>,
    /// The user stopped it (`dto::Error::Cancelled`).
    pub cancelled: bool,
    pub source: dto::Error,
}

impl ErrorResponse {
    pub fn reworded(&self) -> ErrorResponse {
        describe_again(&self.source)
    }
}

/// Which System Settings pane lets the app into `path`: Desktop, Documents,
/// Downloads, iCloud Drive and other disks each have a switch under Files
/// and Folders; anything else macOS keeps private needs Full Disk Access.
pub fn privacy_pane(path: &str) -> dto::PrivacyPane {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        ["Users", _, "Desktop" | "Documents" | "Downloads", ..]
        | ["Users", _, "Library", "Mobile Documents", ..]
        | ["Volumes", ..] => dto::PrivacyPane::FilesAndFolders,
        _ => dto::PrivacyPane::FullDiskAccess,
    }
}

pub fn to_error_response(err: dto::Error) -> ErrorResponse {
    note_shown(&err);
    describe_again(&err)
}

pub fn english_title(err: &dto::Error) -> &'static str {
    english(title_key(err))
}

pub fn note_shown(err: &dto::Error) {
    if !matches!(err, dto::Error::Cancelled) {
        note(english(title_key(err)), Local::now());
    }
}

pub fn describe_again(err: &dto::Error) -> ErrorResponse {
    let device_lost = matches!(
        err,
        dto::Error::Disconnected | dto::Error::NotConnected | dto::Error::Timeout
    );
    let cancelled = matches!(err, dto::Error::Cancelled);
    let privacy = match err {
        dto::Error::FolderAccessDenied(path) => Some(privacy_pane(path)),
        _ => None,
    };
    ErrorResponse {
        title: t(title_key(err)).to_owned(),
        hint: hint(err),
        device_lost,
        privacy,
        cancelled,
        source: err.clone(),
    }
}

fn title_key(err: &dto::Error) -> &'static str {
    match err {
        dto::Error::NoDevice => key("error.no_device.title"),
        dto::Error::MultipleDevices => key("error.multiple_devices.title"),
        dto::Error::PhoneUnplugged => key("error.phone_unplugged.title"),
        dto::Error::DeviceInUse => key("error.device_in_use.title"),
        dto::Error::AndroidFileTransferRunning => key("error.android_file_transfer.title"),
        dto::Error::AllowStorageAccess => key("error.allow_storage_access.title"),
        dto::Error::PermissionDenied => key("error.permission_denied.title"),
        dto::Error::Disconnected => key("error.disconnected.title"),
        dto::Error::Timeout => key("error.timeout.title"),
        dto::Error::NotConnected => key("error.not_connected.title"),
        dto::Error::PathNotFound(_) => key("error.path_not_found.title"),
        dto::Error::FolderAccessDenied(_) => key("error.folder_access_denied.title"),
        dto::Error::InvalidName(dto::NameProblem::Empty) => key("error.name_empty.title"),
        dto::Error::InvalidName(dto::NameProblem::IllegalCharacters) => {
            key("error.name_illegal.title")
        }
        dto::Error::InvalidName(dto::NameProblem::Reserved) => key("error.name_reserved.title"),
        dto::Error::NameTaken(_) => key("error.name_taken.title"),
        dto::Error::ReadOnly => key("error.read_only.title"),
        dto::Error::Unsupported => key("error.unsupported.title"),
        dto::Error::IntoItself(_) => key("error.into_itself.title"),
        dto::Error::Cancelled => key("error.cancelled.title"),
        dto::Error::UnsafeName(_) => key("error.unsafe_name.title"),
        dto::Error::Crashed => key("error.crashed.title"),
        dto::Error::NotEditable => key("error.not_editable.title"),
        dto::Error::OtherPhone => key("error.other_phone.title"),
        dto::Error::TooManyEdits => key("error.too_many_edits.title"),
        dto::Error::TransferTooLarge => key("error.transfer_too_large.title"),
        dto::Error::MacDiskFull => key("error.mac_disk_full.title"),
        dto::Error::NotEnoughRoom { .. } => key("error.not_enough_room.title"),
        dto::Error::VolumeReadOnly(_) => key("error.volume_read_only.title"),
        dto::Error::TooBigForVolume(_) => key("error.too_big_for_volume.title"),
        dto::Error::ChangedSince(_) => key("error.changed_since.title"),
        dto::Error::PartlyUndone(_) => key("error.partly_undone.title"),
        dto::Error::General(_) => key("error.general.title"),
    }
}

fn hint(err: &dto::Error) -> String {
    match err {
        dto::Error::NoDevice => t("error.no_device.hint").to_owned(),
        dto::Error::MultipleDevices => t("error.multiple_devices.hint").to_owned(),
        dto::Error::PhoneUnplugged => t("error.phone_unplugged.hint").to_owned(),
        dto::Error::DeviceInUse => t("error.device_in_use.hint").to_owned(),
        dto::Error::AndroidFileTransferRunning => t("error.android_file_transfer.hint").to_owned(),
        dto::Error::AllowStorageAccess => t("error.allow_storage_access.hint").to_owned(),
        dto::Error::PermissionDenied => t("error.permission_denied.hint").to_owned(),
        dto::Error::Disconnected => t("error.disconnected.hint").to_owned(),
        dto::Error::Timeout => t("error.timeout.hint").to_owned(),
        dto::Error::NotConnected => t("error.not_connected.hint").to_owned(),
        dto::Error::PathNotFound(path) => tf("error.path_not_found.hint", &[("path", path)]),
        dto::Error::FolderAccessDenied(path) => tf(
            "error.folder_access_denied.hint",
            &[("path", path), ("pane", privacy_pane_name(path))],
        ),
        dto::Error::InvalidName(dto::NameProblem::Empty) => t("error.name_empty.hint").to_owned(),
        dto::Error::InvalidName(dto::NameProblem::IllegalCharacters) => {
            t("error.name_illegal.hint").to_owned()
        }
        dto::Error::InvalidName(dto::NameProblem::Reserved) => {
            t("error.name_reserved.hint").to_owned()
        }
        dto::Error::NameTaken(name) => tf("error.name_taken.hint", &[("name", name)]),
        dto::Error::ReadOnly => t("error.read_only.hint").to_owned(),
        dto::Error::Unsupported => t("error.unsupported.hint").to_owned(),
        dto::Error::IntoItself(name) => tf("error.into_itself.hint", &[("name", name)]),
        dto::Error::Cancelled => t("error.cancelled.hint").to_owned(),
        dto::Error::UnsafeName(name) => tf("error.unsafe_name.hint", &[("name", name)]),
        dto::Error::Crashed => t("error.crashed.hint").to_owned(),
        dto::Error::NotEditable => t("error.not_editable.hint").to_owned(),
        dto::Error::OtherPhone => t("error.other_phone.hint").to_owned(),
        dto::Error::TooManyEdits => t("error.too_many_edits.hint").to_owned(),
        dto::Error::TransferTooLarge => t("error.transfer_too_large.hint").to_owned(),
        dto::Error::MacDiskFull => t("error.mac_disk_full.hint").to_owned(),
        dto::Error::NotEnoughRoom { needed, free, disk } => tf(
            "error.not_enough_room.hint",
            &[
                ("needed", &crate::handler::format::format_bytes(*needed)),
                ("free", &crate::handler::format::format_bytes(*free)),
                ("disk", &disk_phrase(disk)),
            ],
        ),
        dto::Error::VolumeReadOnly(disk) => tf(
            "error.volume_read_only.hint",
            &[("disk", &disk_phrase(disk))],
        ),
        dto::Error::TooBigForVolume(disk) => tf(
            "error.too_big_for_volume.hint",
            &[("disk", &disk_phrase(disk))],
        ),
        dto::Error::ChangedSince(name) => tf("error.changed_since.hint", &[("name", name)]),
        dto::Error::PartlyUndone(name) => tf("error.partly_undone.hint", &[("name", name)]),
        dto::Error::General(detail) => tf("error.general.hint", &[("detail", detail)]),
    }
}

fn privacy_pane_name(path: &str) -> &'static str {
    match privacy_pane(path) {
        dto::PrivacyPane::FilesAndFolders => t("error.privacy_pane.files_and_folders"),
        dto::PrivacyPane::FullDiskAccess => t("error.privacy_pane.full_disk_access"),
    }
}

fn disk_phrase(disk: &dto::Disk) -> String {
    match disk {
        dto::Disk::MacDisk => t("error.disk.mac").to_owned(),
        dto::Disk::Volume(name) => tf("error.disk.volume", &[("name", name)]),
        dto::Disk::PhoneStorage(name) => tf("error.disk.phone_storage", &[("name", name)]),
        dto::Disk::Folder(path) => tf("error.disk.folder", &[("path", path)]),
    }
}

/// How many recent problems are kept.
const KEPT: usize = 8;

/// A problem shown: its title, when it was last shown, and how many times in
/// a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub title: &'static str,
    pub at: DateTime<Local>,
    pub times: usize,
}

/// The last few problems shown, oldest first. The same title shown again
/// straight after is counted, not repeated: a failing folder of five
/// thousand photos is one line.
#[derive(Debug, Default)]
pub struct Recent {
    problems: VecDeque<Problem>,
}

impl Recent {
    pub const fn new() -> Self {
        Self {
            problems: VecDeque::new(),
        }
    }

    pub fn note(&mut self, title: &'static str, at: DateTime<Local>) {
        if let Some(last) = self.problems.back_mut()
            && last.title == title
        {
            last.at = at;
            last.times += 1;
            return;
        }
        self.problems.push_back(Problem {
            title,
            at,
            times: 1,
        });
        if self.problems.len() > KEPT {
            self.problems.pop_front();
        }
    }

    pub fn newest_first(&self) -> Vec<Problem> {
        self.problems.iter().rev().cloned().collect()
    }
}

// Errors are mapped on the UI thread and in background tasks alike, so the
// app keeps one history for all of them. Tests run side by side, so each test
// thread keeps its own.
#[cfg(not(test))]
static RECENT: std::sync::Mutex<Recent> = std::sync::Mutex::new(Recent::new());

#[cfg(test)]
thread_local! {
    static RECENT: std::cell::RefCell<Recent> = const { std::cell::RefCell::new(Recent::new()) };
}

fn note(title: &'static str, at: DateTime<Local>) {
    #[cfg(not(test))]
    if let Ok(mut recent) = RECENT.lock() {
        recent.note(title, at);
    }
    #[cfg(test)]
    RECENT.with(|recent| recent.borrow_mut().note(title, at));
}

/// The problems shown recently, newest first.
pub fn recent_problems() -> Vec<Problem> {
    #[cfg(not(test))]
    return RECENT
        .lock()
        .map(|recent| recent.newest_first())
        .unwrap_or_default();
    #[cfg(test)]
    RECENT.with(|recent| recent.borrow().newest_first())
}

/// Starts this test thread's history afresh.
#[cfg(test)]
pub fn clear_recent() {
    RECENT.with(|recent| *recent.borrow_mut() = Recent::new());
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(minute: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 17, 14, minute, 0).unwrap()
    }

    #[test]
    fn a_problem_shown_again_straight_after_is_counted_not_repeated() {
        let mut recent = Recent::new();
        recent.note("Your phone stopped responding", at(1));
        recent.note("Your phone stopped responding", at(2));
        recent.note("This folder is gone", at(3));
        recent.note("Your phone stopped responding", at(4));

        let problems = recent.newest_first();
        let lines: Vec<(&str, usize, DateTime<Local>)> = problems
            .iter()
            .map(|problem| (problem.title, problem.times, problem.at))
            .collect();
        assert_eq!(
            lines,
            [
                ("Your phone stopped responding", 1, at(4)),
                ("This folder is gone", 1, at(3)),
                ("Your phone stopped responding", 2, at(2)),
            ],
            "newest first; only repeats in a row are counted"
        );
    }

    #[test]
    fn only_the_last_eight_are_kept() {
        const TITLES: [&str; 10] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
        let mut recent = Recent::new();
        for (minute, title) in TITLES.iter().enumerate() {
            recent.note(title, at(minute as u32));
        }

        let titles: Vec<&str> = recent.newest_first().iter().map(|p| p.title).collect();
        assert_eq!(titles, ["j", "i", "h", "g", "f", "e", "d", "c"]);
    }

    #[test]
    fn every_error_shown_is_noted_by_its_title_but_a_cancel_is_not() {
        clear_recent();

        let _ = to_error_response(dto::Error::Timeout);
        let _ = to_error_response(dto::Error::Timeout);
        let _ = to_error_response(dto::Error::Cancelled);
        let _ = to_error_response(dto::Error::PathNotFound("/DCIM/Camera/IMG_1.jpg".into()));

        let problems = recent_problems();
        let lines: Vec<(&str, usize)> = problems.iter().map(|p| (p.title, p.times)).collect();
        assert_eq!(
            lines,
            [
                ("This folder is gone", 1),
                ("Your phone stopped responding", 2)
            ],
            "the path stays in the hint, out of the history"
        );
    }

    #[test]
    fn general_error_keeps_the_device_detail_in_the_hint() {
        let response = to_error_response(dto::Error::General("InvalidParameter".into()));

        assert_eq!(response.title, "Something went wrong talking to your phone");
        assert!(response.hint.ends_with("Details: InvalidParameter"));
    }
}

#[cfg(test)]
mod every_error {
    use super::*;

    fn all() -> Vec<dto::Error> {
        vec![
            dto::Error::NoDevice,
            dto::Error::MultipleDevices,
            dto::Error::DeviceInUse,
            dto::Error::AndroidFileTransferRunning,
            dto::Error::AllowStorageAccess,
            dto::Error::PermissionDenied,
            dto::Error::Disconnected,
            dto::Error::Timeout,
            dto::Error::NotConnected,
            dto::Error::PathNotFound("/DCIM".into()),
            dto::Error::FolderAccessDenied("/Users/me/Desktop".into()),
            dto::Error::ChangedSince("Trip".into()),
            dto::Error::PartlyUndone("Trip 001.jpg".into()),
            dto::Error::NotEnoughRoom {
                needed: 2,
                free: 1,
                disk: dto::Disk::MacDisk,
            },
            dto::Error::InvalidName(dto::NameProblem::Empty),
            dto::Error::InvalidName(dto::NameProblem::IllegalCharacters),
            dto::Error::InvalidName(dto::NameProblem::Reserved),
            dto::Error::NameTaken("Trip".into()),
            dto::Error::ReadOnly,
            dto::Error::Unsupported,
            dto::Error::IntoItself("Trip".into()),
            dto::Error::Cancelled,
            dto::Error::MacDiskFull,
            dto::Error::VolumeReadOnly(dto::Disk::Volume("BACKUP".into())),
            dto::Error::TooBigForVolume(dto::Disk::Volume("KINGSTON".into())),
            dto::Error::UnsafeName("../x".into()),
            dto::Error::TransferTooLarge,
            dto::Error::General("x".into()),
        ]
    }

    #[test]
    fn has_a_title_and_a_next_step() {
        for err in all() {
            let response = to_error_response(err.clone());
            assert!(!response.title.is_empty(), "{err:?} has no title");
            assert!(!response.hint.is_empty(), "{err:?} has no hint");
        }
    }

    #[test]
    fn only_a_missing_or_silent_phone_counts_as_device_lost() {
        let lost: Vec<dto::Error> = all()
            .into_iter()
            .filter(|err| to_error_response(err.clone()).device_lost)
            .collect();

        assert_eq!(
            lost,
            [
                dto::Error::Disconnected,
                dto::Error::Timeout,
                dto::Error::NotConnected
            ]
        );
    }

    #[test]
    fn path_not_found_names_the_folder() {
        let response = to_error_response(dto::Error::PathNotFound("/DCIM".into()));

        assert!(response.hint.starts_with("/DCIM was moved or deleted"));
    }

    #[test]
    fn folder_access_denied_points_at_privacy_settings() {
        let response =
            to_error_response(dto::Error::FolderAccessDenied("/Users/me/Desktop".into()));

        assert!(response.hint.contains("/Users/me/Desktop"));
        assert!(response.hint.contains("Files and Folders"));
        assert!(!response.device_lost);
        assert_eq!(response.privacy, Some(dto::PrivacyPane::FilesAndFolders));

        let mail = to_error_response(dto::Error::FolderAccessDenied(
            "/Users/me/Library/Mail".into(),
        ));
        assert!(mail.hint.contains("Full Disk Access"), "{}", mail.hint);
        assert_eq!(mail.privacy, Some(dto::PrivacyPane::FullDiskAccess));
        assert_eq!(to_error_response(dto::Error::Timeout).privacy, None);
    }

    #[test]
    fn a_read_only_disk_and_a_fat32_disk_say_which_and_what_to_do() {
        let read_only = to_error_response(dto::Error::VolumeReadOnly(dto::Disk::Volume(
            "BACKUP".into(),
        )));
        assert_eq!(read_only.title, "That disk is read-only");
        assert!(
            read_only
                .hint
                .contains("Nothing can be written on “BACKUP”")
        );
        assert!(read_only.hint.contains("NTFS"));
        let too_big = to_error_response(dto::Error::TooBigForVolume(dto::Disk::Volume(
            "KINGSTON".into(),
        )));
        assert_eq!(too_big.title, "4 GB or more: too big for a FAT32 disk");
        assert!(
            too_big
                .hint
                .contains("on “KINGSTON”: it is formatted FAT32")
        );
        assert!(too_big.hint.contains("exFAT"));
    }

    #[test]
    fn a_transfer_that_would_not_fit_says_how_much_it_needs_and_where() {
        let response = to_error_response(dto::Error::NotEnoughRoom {
            needed: 12_400_000_000,
            free: 4_100_000_000,
            disk: dto::Disk::Volume("USB STICK".into()),
        });

        assert_eq!(response.title, "Not enough room");
        assert!(
            response
                .hint
                .starts_with("Needs 12.4 GB; 4.1 GB is free on “USB STICK”."),
            "{}",
            response.hint
        );
        assert!(!response.device_lost);
    }

    #[test]
    fn each_private_folder_is_let_in_from_its_own_settings_pane() {
        use dto::PrivacyPane::{FilesAndFolders, FullDiskAccess};
        for (path, pane) in [
            ("/Users/me/Desktop", FilesAndFolders),
            ("/Users/me/Documents/Taxes", FilesAndFolders),
            ("/Users/me/Downloads/", FilesAndFolders),
            (
                "/Users/me/Library/Mobile Documents/com~apple~CloudDocs",
                FilesAndFolders,
            ),
            ("/Volumes/USB STICK/DCIM", FilesAndFolders),
            ("/Users/me/Library/Mail", FullDiskAccess),
            ("/Users/someone-else", FullDiskAccess),
            (
                "/Users/me/Pictures/Photos Library.photoslibrary",
                FullDiskAccess,
            ),
            ("/Library/Application Support/com.apple.TCC", FullDiskAccess),
        ] {
            assert_eq!(privacy_pane(path), pane, "{path}");
        }
    }

    #[test]
    fn a_taken_name_quotes_the_name() {
        let response = to_error_response(dto::Error::NameTaken("Trip".into()));

        assert!(response.hint.starts_with("“Trip” already exists"));
    }

    #[test]
    fn illegal_characters_lists_them() {
        let response =
            to_error_response(dto::Error::InvalidName(dto::NameProblem::IllegalCharacters));

        assert!(response.hint.contains(r#"/ \ ? * : | " < >"#));
        assert!(!response.hint.contains('%'), "% is allowed now");
    }
}
