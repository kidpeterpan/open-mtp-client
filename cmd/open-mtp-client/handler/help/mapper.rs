use internal::dto;

use super::request::Link;
use super::response::{ShortcutGroup, ShortcutLine};
use crate::handler::error::Problem;
use crate::keymap::Row;

/// The Shortcuts tab: the keymap's lines, grouped in the order the groups first
/// appear, each line's alternatives split into separate keys.
pub fn to_shortcut_groups(rows: &[Row]) -> Vec<ShortcutGroup> {
    let mut groups: Vec<ShortcutGroup> = Vec::new();
    for row in rows {
        let line = ShortcutLine {
            label: row.label.to_owned(),
            keys: row.keys.split(" or ").map(str::to_owned).collect(),
        };
        match groups.iter_mut().find(|group| group.title == row.group) {
            Some(group) => group.lines.push(line),
            None => groups.push(ShortcutGroup {
                title: row.group.to_owned(),
                lines: vec![line],
            }),
        }
    }
    groups
}

/// Things to do with the mouse, which have no key to list.
pub fn mouse_tips() -> [(&'static str, &'static str); 9] {
    [
        ("Right-click", "a menu of what you can do with the file"),
        ("Double-click", "open a folder"),
        ("⌘-click", "add or remove one item from the selection"),
        ("⇧-click", "select everything from the last click to here"),
        (
            "Drag beside the names",
            "draw a box to select; ⌘ turns items over, ⇧ adds",
        ),
        (
            "Drag onto a folder",
            "move the selection into it; hold ⌥ to copy",
        ),
        (
            "Drag to the other pane",
            "copy there, or into a folder in it; hold ⌘ to move",
        ),
        (
            "Drag from Finder",
            "drop files on the phone's pane to upload them",
        ),
        (
            "Column heads",
            "click Name, Size or Modified to sort; again to reverse",
        ),
    ]
}

impl Link {
    pub fn url(self) -> &'static str {
        match self {
            Link::NewIssue => "https://github.com/kidpeterpan/open-mtp-client/issues/new",
            Link::Project => "https://github.com/kidpeterpan/open-mtp-client",
            Link::Support => "https://buymeacoffee.com/kidpeterpan",
        }
    }
}

/// The text a support report holds: what the app is, the Mac, the phone as the
/// app sees it, and what the connection check found. It contains no file
/// names, paths, photos or account details, and the Help dialog shows it in
/// full before anyone copies it.
/// What the report says about the phone: never a path or a name.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportPhone {
    /// "Pixel 9, serial …1234, 2 storages".
    pub summary: String,
    pub abilities: Option<dto::Abilities>,
    /// The USB link, when known.
    pub link: Option<dto::UsbSpeed>,
    /// A timed reading made in Troubleshooting (how many, how long; the
    /// folder is left out).
    pub reading: Option<dto::ReadTiming>,
}

pub fn report_text(
    version: &str,
    mac: &dto::MacInfo,
    phone: &ReportPhone,
    diagnosis: &dto::Diagnosis,
    problems: &[Problem],
) -> String {
    let mut phone_lines = format!("Phone: {}\n", phone.summary);
    if let Some(abilities) = &phone.abilities {
        phone_lines.push_str(&format!(
            "Phone operations: {}\n",
            crate::handler::format::operations_line(abilities)
        ));
    }
    if let Some(link) = phone.link {
        phone_lines.push_str(&format!(
            "USB link: {}, {} Mb/s\n",
            crate::handler::format::link_label(link),
            link.megabits()
        ));
    }
    if let Some(reading) = &phone.reading {
        phone_lines.push_str(&format!(
            "Reading a folder: {}\n",
            crate::handler::format::format_reading(reading)
        ));
    }
    let os = if mac.os_version.is_empty() {
        "macOS (version unknown)".to_owned()
    } else {
        format!("macOS {}", mac.os_version)
    };
    let usb = match diagnosis.phones.as_deref() {
        None => "not checked".to_owned(),
        Some([]) => "no phone seen".to_owned(),
        Some(phones) => phones.join(", "),
    };
    let apps = if diagnosis.owner_seen {
        // macOS's own record: who has it, by name, a program the app doesn't
        // know by its process name (never a path), worth adding to the list.
        let names: Vec<String> = diagnosis
            .holders
            .iter()
            .flatten()
            .map(|holder| holder.name.clone())
            .chain(
                diagnosis
                    .unknown_owners
                    .iter()
                    .map(|process| format!("“{process}” (not known to the app)")),
            )
            .collect();
        format!(
            "Holding the phone, as macOS records it: {}",
            names.join(", ")
        )
    } else {
        let apps = match diagnosis.holders.as_deref() {
            None => "not checked".to_owned(),
            Some([]) => "none running".to_owned(),
            Some(holders) => holders
                .iter()
                .map(|holder| holder.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        };
        format!("Apps that can hold a phone: {apps}")
    };
    // Titles only (fixed text, never a path or a name), with when and how
    // often in a row.
    let recent = if problems.is_empty() {
        "Recent problems: none this session\n".to_owned()
    } else {
        let mut lines = "Recent problems (this session, newest first):\n".to_owned();
        for problem in problems {
            let times = match problem.times {
                1 => String::new(),
                n => format!(" (×{n})"),
            };
            lines.push_str(&format!(
                "  {}  {}{times}\n",
                problem.at.format("%H:%M"),
                problem.title
            ));
        }
        lines
    };
    let crashes = crash_lines(&mac.crashes);
    format!(
        "open-mtp-client {version}\n{os} ({arch}{rosetta})\n{phone_lines}Seen on USB: {usb}\n{apps}\n{recent}{crashes}",
        arch = mac.arch,
        // The universal app runs as x86_64 on Apple Silicon only when someone
        // asked for Rosetta: slower than it needs to be, worth knowing.
        rosetta = if mac.translated { " under Rosetta" } else { "" },
    )
}

/// Reports of the app's own failures: when and where in the code, as the
/// notes hold them, never what a report says (a panic's message can hold a
/// file's name) and never where the file is.
fn crash_lines(crashes: &[dto::CrashNote]) -> String {
    use chrono::{DateTime, Local};

    /// The newest few; the rest are only counted.
    const SHOWN: usize = 5;
    if crashes.is_empty() {
        return "Crashes: none kept\n".to_owned();
    }
    let mut lines = "Crashes kept (newest first):\n".to_owned();
    for note in crashes.iter().take(SHOWN) {
        let at = i64::try_from(note.at_ms)
            .ok()
            .and_then(DateTime::from_timestamp_millis)
            .map_or_else(
                || "unknown time".to_owned(),
                |at| {
                    at.with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                },
            );
        let place = if note.location.is_empty() {
            "unknown place"
        } else {
            note.location.as_str()
        };
        lines.push_str(&format!("  {at}  {place}\n"));
    }
    if crashes.len() > SHOWN {
        lines.push_str(&format!("  and {} more\n", crashes.len() - SHOWN));
    }
    lines
}

/// The Terminal line that links the bundled command-line tool at `path`
/// into the usual folder of commands (in every Mac's path). The path is
/// quoted for the shell, so a space or a quote in it can't break the line.
pub fn cli_link_line(path: &str) -> String {
    format!(
        "ln -sf {} /usr/local/bin/open-mtp-client-cli",
        shell_quote(path)
    )
}

/// `text` in single quotes, a single quote in it closed, escaped and
/// reopened, which leaves nothing for the shell to read into it.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_line_quotes_the_path_so_nothing_in_it_reaches_the_shell() {
        assert_eq!(
            cli_link_line("/Applications/open-mtp-client.app/Contents/MacOS/open-mtp-client-cli"),
            "ln -sf '/Applications/open-mtp-client.app/Contents/MacOS/open-mtp-client-cli' /usr/local/bin/open-mtp-client-cli"
        );
        assert_eq!(
            cli_link_line("/Users/me/My Apps/it's; rm -rf ~"),
            "ln -sf '/Users/me/My Apps/it'\\''s; rm -rf ~' /usr/local/bin/open-mtp-client-cli",
            "a space and a quote stay in the path"
        );
    }
    use crate::keymap;

    fn mac() -> dto::MacInfo {
        dto::MacInfo {
            os_version: "15.1".into(),
            arch: "aarch64".into(),
            translated: false,
            ..Default::default()
        }
    }

    #[test]
    fn shortcuts_are_grouped_in_the_order_they_first_appear() {
        let groups = to_shortcut_groups(&keymap::catalog());

        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Moving around",
                "Selecting",
                "Changing files",
                "Copying",
                "The phone",
                "The app"
            ]
        );
    }

    #[test]
    fn every_catalog_line_reaches_the_tab_with_its_keys_split() {
        let rows = keymap::catalog();
        let groups = to_shortcut_groups(&rows);

        let lines: usize = groups.iter().map(|g| g.lines.len()).sum();
        assert_eq!(lines, rows.len());
        let up = groups
            .iter()
            .flat_map(|g| &g.lines)
            .find(|line| line.label == "Go up one folder")
            .unwrap();
        assert_eq!(up.keys, ["⌘↑", "⌘B"]);
        let copy = groups
            .iter()
            .flat_map(|g| &g.lines)
            .find(|line| line.label == "Copy the selection")
            .unwrap();
        assert_eq!(copy.keys, ["⌘C"]);
    }

    #[test]
    fn no_rows_means_no_groups() {
        assert!(to_shortcut_groups(&[]).is_empty());
    }

    #[test]
    fn mouse_tips_say_what_each_gesture_does() {
        for (gesture, does) in mouse_tips() {
            assert!(!gesture.is_empty() && !does.is_empty());
        }
    }

    #[test]
    fn links_are_plain_https_addresses_to_the_project_or_its_coffee_page() {
        assert_eq!(Link::Support.url(), "https://buymeacoffee.com/kidpeterpan");
        for link in [Link::NewIssue, Link::Project] {
            let url = link.url();
            assert!(
                url.starts_with("https://github.com/kidpeterpan/open-mtp-client"),
                "{url}"
            );
            assert!(!url.contains(char::is_whitespace));
        }
        assert!(Link::NewIssue.url().ends_with("/issues/new"));
    }

    #[test]
    fn a_report_says_what_the_app_the_mac_and_the_phone_are() {
        let diagnosis = dto::Diagnosis {
            phones: Some(vec!["Pixel 7".into()]),
            holders: Some(vec![
                dto::Holder {
                    name: "Photos".into(),
                    restarts: false,
                },
                dto::Holder {
                    name: "Preview".into(),
                    restarts: false,
                },
            ]),
            ..Default::default()
        };

        let text = report_text(
            "0.1.0",
            &mac(),
            &phone("Pixel 9, serial …1234, 2 storages"),
            &diagnosis,
            &[],
        );

        assert_eq!(
            text,
            "open-mtp-client 0.1.0\nmacOS 15.1 (aarch64)\nPhone: Pixel 9, serial …1234, 2 storages\nSeen on USB: Pixel 7\nApps that can hold a phone: Photos, Preview\nRecent problems: none this session\nCrashes: none kept\n"
        );
    }

    #[test]
    fn a_report_names_who_macos_says_has_the_phone() {
        let text = report_text(
            "0.1.0",
            &mac(),
            &phone("not connected"),
            &dto::Diagnosis {
                holders: Some(vec![dto::Holder {
                    name: "Garmin Express".into(),
                    restarts: false,
                }]),
                owner_seen: true,
                unknown_owners: vec!["SyncMate".into()],
                ..Default::default()
            },
            &[],
        );

        assert!(
            text.contains(
                "\nHolding the phone, as macOS records it: Garmin Express, “SyncMate” (not known to the app)\n"
            ),
            "{text}"
        );
        assert!(!text.contains("Apps that can hold a phone"));
    }

    fn crash(at_ms: u64, location: &str, file: Option<&str>) -> dto::CrashNote {
        dto::CrashNote {
            at_ms,
            location: location.to_owned(),
            reported: false,
            file: file.map(std::path::PathBuf::from),
        }
    }

    fn report_with(crashes: Vec<dto::CrashNote>) -> String {
        report_text(
            "0.1.0",
            &dto::MacInfo { crashes, ..mac() },
            &phone("x"),
            &dto::Diagnosis::default(),
            &[],
        )
    }

    #[test]
    fn a_report_says_when_no_crash_was_kept() {
        assert!(report_with(Vec::new()).contains("\nCrashes: none kept\n"));
    }

    #[test]
    fn a_report_lists_the_crashes_by_when_and_where_in_the_code() {
        let text = report_with(vec![
            crash(
                1_792_000_000_000,
                "internal/service/x.rs:7:3",
                Some("/Users/me/crash-1.txt"),
            ),
            crash(1_791_000_000_000, "", None),
        ]);

        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "Crashes kept (newest first):")
            .unwrap();
        assert!(
            lines[at + 1].ends_with("  internal/service/x.rs:7:3"),
            "{}",
            lines[at + 1]
        );
        assert!(
            lines[at + 2].ends_with("  unknown place"),
            "{}",
            lines[at + 2]
        );
        // "2026-10-18 08:30": the date and the time, to the minute.
        assert!(
            lines[at + 1].trim_start().starts_with("2026-10-"),
            "{}",
            lines[at + 1]
        );
    }

    #[test]
    fn a_report_never_holds_where_a_crash_report_is_kept() {
        let text = report_with(vec![crash(
            1_792_000_000_000,
            "internal/x.rs:1:1",
            Some("/Users/someone/Library/Application Support/open-mtp-client/crashes/crash-1.txt"),
        )]);

        assert!(!text.contains("/Users"), "{text}");
        assert!(!text.contains("crash-1.txt"), "{text}");
        assert!(!text.contains("Application Support"), "{text}");
    }

    #[test]
    fn a_report_shows_the_newest_few_crashes_and_counts_the_rest() {
        let crashes: Vec<dto::CrashNote> = (0..8)
            .map(|n| crash(1_792_000_000_000 - n, "internal/x.rs:1:1", None))
            .collect();

        let text = report_with(crashes);

        assert_eq!(text.matches("  internal/x.rs:1:1\n").count(), 5);
        assert!(text.contains("  and 3 more\n"));
    }

    #[test]
    fn a_report_says_when_the_app_runs_under_rosetta() {
        let text = report_text(
            "0.1.0",
            &dto::MacInfo {
                os_version: "15.1".into(),
                arch: "x86_64".into(),
                translated: true,
                ..Default::default()
            },
            &phone("x"),
            &dto::Diagnosis::default(),
            &[],
        );

        assert!(
            text.contains("macOS 15.1 (x86_64 under Rosetta)\n"),
            "{text}"
        );
    }

    #[test]
    fn a_report_with_nothing_found_says_so_in_plain_words() {
        let text = report_text(
            "0.1.0",
            &mac(),
            &phone("not connected"),
            &dto::Diagnosis {
                phones: Some(vec![]),
                holders: Some(vec![]),
                ..Default::default()
            },
            &[],
        );

        assert!(text.contains("Seen on USB: no phone seen\n"));
        assert!(text.contains("Apps that can hold a phone: none running\n"));
    }

    #[test]
    fn a_report_with_unchecked_parts_says_not_checked() {
        let text = report_text(
            "0.1.0",
            &mac(),
            &phone("x"),
            &dto::Diagnosis::default(),
            &[],
        );

        assert!(text.contains("Seen on USB: not checked\n"));
        assert!(text.contains("Apps that can hold a phone: not checked\n"));
    }

    #[test]
    fn an_unreadable_mac_version_does_not_leave_a_gap() {
        let text = report_text(
            "0.1.0",
            &dto::MacInfo {
                os_version: String::new(),
                arch: "x86_64".into(),
                translated: false,
                ..Default::default()
            },
            &phone("x"),
            &dto::Diagnosis::default(),
            &[],
        );

        assert!(text.contains("macOS (version unknown) (x86_64)\n"));
    }

    fn phone(summary: &str) -> ReportPhone {
        ReportPhone {
            summary: summary.to_owned(),
            abilities: None,
            link: None,
            reading: None,
        }
    }

    #[test]
    fn a_report_names_the_phones_operations_and_a_timed_reading_without_its_folder() {
        let phone = ReportPhone {
            summary: "Pixel 9, serial …1234, 1 storage".into(),
            abilities: Some(dto::Abilities {
                copy: true,
                partial_reads: true,
                partial_reads_64: true,
                ..dto::Abilities::default()
            }),
            link: Some(dto::UsbSpeed::High),
            reading: Some(dto::ReadTiming {
                items: 312,
                took: std::time::Duration::from_millis(1_430),
            }),
        };

        let text = report_text("0.1.0", &mac(), &phone, &dto::Diagnosis::default(), &[]);

        assert!(
            text.contains(
                "Phone operations: CopyObject yes, MoveObject no, rename no, GetPartialObject yes, GetPartialObject64 yes, GetThumb no, GetObjectPropList no, events no\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("Reading a folder: 312 items in 1.4 s\n"),
            "{text}"
        );
        assert!(text.contains("USB link: USB 2, 480 Mb/s\n"), "{text}");
        let plain = report_text(
            "0.1.0",
            &mac(),
            &self::phone("x"),
            &dto::Diagnosis::default(),
            &[],
        );
        assert!(
            !plain.contains("Phone operations")
                && !plain.contains("Reading a folder")
                && !plain.contains("USB link")
        );
    }

    fn problems() -> Vec<Problem> {
        use chrono::TimeZone;
        let at = |minute| {
            chrono::Local
                .with_ymd_and_hms(2026, 10, 17, 14, minute, 0)
                .unwrap()
        };
        vec![
            Problem {
                title: "Your phone stopped responding",
                at: at(2),
                times: 3,
            },
            Problem {
                title: "This folder is gone",
                at: at(58),
                times: 1,
            },
        ]
    }

    #[test]
    fn a_report_lists_the_recent_problems_newest_first_with_times() {
        let text = report_text(
            "0.1.0",
            &mac(),
            &phone("x"),
            &dto::Diagnosis::default(),
            &problems(),
        );

        assert!(
            text.contains(
                "Recent problems (this session, newest first):\n  14:02  Your phone stopped responding (×3)\n  14:58  This folder is gone\nCrashes: none kept\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_report_holds_no_paths_or_file_names() {
        let text = report_text(
            "0.1.0",
            &mac(),
            &phone("Pixel 9, serial …1234, 2 storages"),
            &dto::Diagnosis::default(),
            &problems(),
        );

        assert!(!text.contains('/'), "{text}");
        assert!(!text.contains("Users"));
    }
}
