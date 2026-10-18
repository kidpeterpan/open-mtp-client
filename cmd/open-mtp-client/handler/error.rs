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

/// Turns a failure into what the user sees: a short title plus one concrete
/// next step they can take.
pub fn to_error_response(err: dto::Error) -> ErrorResponse {
    // A phone that stopped answering is as good as gone: its session can't be
    // trusted, and reconnecting opens a fresh one.
    let device_lost = matches!(
        err,
        dto::Error::Disconnected | dto::Error::NotConnected | dto::Error::Timeout
    );
    // The user stopped it: not a problem.
    let cancelled = matches!(err, dto::Error::Cancelled);
    let worth_noting = !cancelled;
    let privacy = match &err {
        dto::Error::FolderAccessDenied(path) => Some(privacy_pane(path)),
        _ => None,
    };
    let (title, hint) = match err {
        dto::Error::NoDevice => (
            "No device found",
            "Plug your phone in with a USB cable that carries data (some are charge-only), unlock it, then try again.".to_owned(),
        ),
        dto::Error::MultipleDevices => (
            "Several phones are plugged in",
            "Pick the one to open from the list.".to_owned(),
        ),
        dto::Error::PhoneUnplugged => (
            "That phone is no longer plugged in",
            "Plug it in again, or pick another phone from the list.".to_owned(),
        ),
        dto::Error::DeviceInUse => (
            "Another app is using your phone",
            "Quit apps that talk to phones, such as Image Capture or Photos, then try again.".to_owned(),
        ),
        dto::Error::AndroidFileTransferRunning => (
            "Android File Transfer is in the way",
            "Quit Android File Transfer, including “Android File Transfer Agent” in Activity Monitor, then try again.".to_owned(),
        ),
        dto::Error::AllowStorageAccess => (
            "Allow access on your phone",
            "Unlock your phone, tap the USB notification and choose “File transfer”. If it asks to allow access to phone data, tap Allow.".to_owned(),
        ),
        dto::Error::PermissionDenied => (
            "macOS blocked access to the device",
            "Unplug the phone and plug it back in. If this keeps happening, restart your Mac.".to_owned(),
        ),
        dto::Error::Disconnected => (
            "Your phone was disconnected",
            "Check the cable, plug the phone back in and keep it unlocked while it's connected.".to_owned(),
        ),
        dto::Error::Timeout => (
            "Your phone stopped responding",
            "Unlock the phone and keep the screen on, then press Reconnect. If that doesn't help, unplug it and plug it back in.".to_owned(),
        ),
        dto::Error::NotConnected => (
            "No device connected",
            "Connect your phone, then press Try again.".to_owned(),
        ),
        dto::Error::PathNotFound(path) => (
            "This folder is gone",
            format!("{path} was moved or deleted on the phone. Pick another folder from the path above."),
        ),
        dto::Error::FolderAccessDenied(path) => (
            "open-mtp-client can't open this folder",
            format!(
                "macOS hasn't given open-mtp-client access to {path}. Allow it in System Settings › Privacy & Security › {}; the folder opens when you come back.",
                match privacy_pane(&path) {
                    dto::PrivacyPane::FilesAndFolders => "Files and Folders",
                    dto::PrivacyPane::FullDiskAccess => "Full Disk Access",
                }
            ),
        ),
        dto::Error::InvalidName(dto::NameProblem::Empty) => (
            "Enter a name",
            "Type a name, then press Return.".to_owned(),
        ),
        dto::Error::InvalidName(dto::NameProblem::IllegalCharacters) => (
            "That name has characters phones can't use",
            "Remove any of / \\ ? * : | \" < > from the name.".to_owned(),
        ),
        dto::Error::InvalidName(dto::NameProblem::Reserved) => (
            "“.” and “..” are reserved",
            "Choose a different name.".to_owned(),
        ),
        dto::Error::NameTaken(name) => (
            "That name is taken",
            format!("“{name}” already exists here. Choose a different name."),
        ),
        dto::Error::ReadOnly => (
            "This storage is read-only",
            "Files here can be copied but not changed. Pick another storage to make changes.".to_owned(),
        ),
        dto::Error::Unsupported => (
            "Your phone can't do this over USB",
            "Do it on the phone itself, then press Refresh.".to_owned(),
        ),
        dto::Error::IntoItself(name) => (
            "A folder can't go inside itself",
            format!("“{name}” can't be copied or moved into itself or into a folder inside it. Pick a different folder."),
        ),
        dto::Error::Cancelled => (
            "Cancelled",
            "Files that finished are kept. Retry carries on with the rest, skipping what is already there.".to_owned(),
        ),
        dto::Error::UnsafeName(name) => (
            "Copy stopped for safety",
            format!("The phone reported a file named “{name}”, which could write outside the folder you chose. Nothing was copied."),
        ),
        dto::Error::Crashed => (
            "Something went wrong inside the app",
            "What it was doing was stopped, and the app is still running. A crash report was saved on this Mac, and the banner below the panes can open it. Try again; if it keeps happening, send the report.".to_owned(),
        ),
        dto::Error::NotEditable => (
            "Only a file can be edited on the Mac",
            "Pick one file of up to 1 GB whose size the phone gives.".to_owned(),
        ),
        dto::Error::OtherPhone => (
            "This is for another phone",
            "Connect the phone it came from, then send it again. Nothing was sent.".to_owned(),
        ),
        dto::Error::TooManyEdits => (
            "20 files are being edited already",
            "Send or discard one of them in the transfers below, then try again.".to_owned(),
        ),
        dto::Error::TransferTooLarge => (
            "Too much to copy at once",
            "The selection is nested too deeply or holds too many items. Copy its folders one at a time.".to_owned(),
        ),
        dto::Error::MacDiskFull => (
            "Your Mac's disk is full",
            "Free up some space, then try again.".to_owned(),
        ),
        dto::Error::NotEnoughRoom {
            needed,
            free,
            place,
        } => (
            "Not enough room",
            format!(
                "Needs {}; {} is free {place}. Nothing was copied: make room, or choose fewer items.",
                crate::handler::format::format_bytes(needed),
                crate::handler::format::format_bytes(free),
            ),
        ),
        dto::Error::VolumeReadOnly(place) => (
            "That disk is read-only",
            format!("Nothing can be written {place}. A Mac only reads NTFS drives, and a locked SD card or a read-only disk image can't be written either. Nothing was copied: choose a folder on another disk."),
        ),
        dto::Error::TooBigForVolume(place) => (
            "4 GB or more: too big for a FAT32 disk",
            format!("Files of 4 GB or more can't be saved {place}: it is formatted FAT32 (MS-DOS). The rest were copied. Use a disk formatted APFS or exFAT for these."),
        ),
        dto::Error::ChangedSince(name) => (
            "Can't undo: it has changed since",
            format!("“{name}” isn't as the last change left it, so nothing was undone."),
        ),
        dto::Error::PartlyUndone(name) => (
            "Undid the rename, but not all of it",
            format!("“{name}” isn't as the rename left it, so it keeps its new name; the others have their old names back."),
        ),
        dto::Error::General(detail) => (
            "Something went wrong talking to your phone",
            format!("Unplug the phone and plug it back in, then try again. Details: {detail}"),
        ),
    };

    if worth_noting {
        note(title, Local::now());
    }
    ErrorResponse {
        title: title.to_owned(),
        hint,
        device_lost,
        privacy,
        cancelled,
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
                place: "on your Mac's disk".into(),
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
            dto::Error::VolumeReadOnly("on “BACKUP”".into()),
            dto::Error::TooBigForVolume("on “KINGSTON”".into()),
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
        let read_only = to_error_response(dto::Error::VolumeReadOnly("on “BACKUP”".into()));
        assert_eq!(read_only.title, "That disk is read-only");
        assert!(
            read_only
                .hint
                .contains("Nothing can be written on “BACKUP”")
        );
        assert!(read_only.hint.contains("NTFS"));
        let too_big = to_error_response(dto::Error::TooBigForVolume("on “KINGSTON”".into()));
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
            place: "on “USB STICK”".into(),
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
