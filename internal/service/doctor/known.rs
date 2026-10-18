//! What is known about apps that grab phones over USB, and about who makes
//! phones. Both lists come from users' reports of phones that would not
//! connect.

/// How to make a running app let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    /// Ask it to quit, like ⌘Q: documents get their save prompts.
    App,
    /// A background process with no window: end it.
    Kill,
}

/// A program that may hold a phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Known {
    /// The process name `ps` shows, which is also the app's name to AppleScript.
    pub process: &'static str,
    /// What the user sees.
    pub name: &'static str,
    pub quit: Quit,
    /// macOS starts it again when a phone is plugged in.
    pub restarts: bool,
}

const fn app(process: &'static str, name: &'static str) -> Known {
    Known {
        process,
        name,
        quit: Quit::App,
        restarts: false,
    }
}

/// What the user is told macOS's camera services are called.
pub const CAMERA_SERVICES: &str = "macOS's camera service";

/// macOS's own programs that claim a phone as a camera: they ignore a polite
/// quit, so freeing the phone from them means ending them outright.
pub const CAMERA_PROCESSES: [&str; 2] = ["ptpcamerad", "mscamerad-xpc"];

/// Most likely culprit first: that is the order the user sees them in.
pub const KNOWN: &[Known] = &[
    Known {
        process: "Android File Transfer Agent",
        name: "Android File Transfer",
        quit: Quit::Kill,
        restarts: false,
    },
    app("Android File Transfer", "Android File Transfer"),
    // macOS's own camera service claims anything that looks like a camera,
    // which includes phones, and relaunches itself when one is plugged in.
    Known {
        process: "ptpcamerad",
        name: CAMERA_SERVICES,
        quit: Quit::Kill,
        restarts: true,
    },
    // Its Image Capture helper holds the phone's interface too;
    // one holder with the service, to the user.
    Known {
        process: "mscamerad-xpc",
        name: CAMERA_SERVICES,
        quit: Quit::Kill,
        restarts: true,
    },
    app("Photos", "Photos"),
    app("Image Capture", "Image Capture"),
    app("Preview", "Preview"),
    app("Smart Switch", "Samsung Smart Switch"),
    app("HandShaker", "HandShaker"),
    app("Google Drive", "Google Drive"),
    app("Dropbox", "Dropbox"),
    app("OneDrive", "OneDrive"),
    app("Adobe Lightroom Classic", "Lightroom Classic"),
    app("Adobe Lightroom", "Lightroom"),
    app("iMovie", "iMovie"),
    // Apps users have reported keeping the phone: looked for only when macOS
    // doesn't say who has it.
    app("Garmin Express", "Garmin Express"),
    // It starts with the Mac and keeps running with no window.
    app("Garmin Express Service", "Garmin Express"),
    app("MightyText", "MightyText"),
    app("Commander One", "Commander One"),
    app("MacDroid", "MacDroid"),
    app("Resolve", "DaVinci Resolve"),
    app("Nikon Transfer 2", "Nikon Transfer"),
];

/// USB vendor ids of companies that mostly make phones. A heuristic: a phone
/// from another maker is simply not mentioned, and makers that also sell
/// keyboards, headphones or hubs are left out so those are not mistaken for
/// phones.
const PHONE_VENDORS: &[(u16, &str)] = &[
    (0x18d1, "Google"),
    (0x04e8, "Samsung"),
    (0x2717, "Xiaomi"),
    (0x22b8, "Motorola"),
    (0x12d1, "Huawei"),
    (0x2a70, "OnePlus"),
    (0x0bb4, "HTC"),
    (0x1004, "LG"),
    (0x0fce, "Sony"),
    (0x22d9, "OPPO"),
    (0x2d95, "vivo"),
    (0x19d2, "ZTE"),
    (0x2e04, "Nokia"),
    (0x1bbb, "Alcatel"),
];

pub fn phone_vendor(vendor_id: u16) -> Option<&'static str> {
    PHONE_VENDORS
        .iter()
        .find(|(id, _)| *id == vendor_id)
        .map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apps_users_reported_holding_a_phone_are_known() {
        for (process, name) in [
            ("Garmin Express", "Garmin Express"),
            ("Garmin Express Service", "Garmin Express"),
            ("MightyText", "MightyText"),
            ("Commander One", "Commander One"),
            ("MacDroid", "MacDroid"),
            ("Resolve", "DaVinci Resolve"),
            ("Nikon Transfer 2", "Nikon Transfer"),
        ] {
            let known = KNOWN.iter().find(|known| known.process == process);
            assert_eq!(
                known.map(|known| (known.name, known.quit, known.restarts)),
                Some((name, Quit::App, false)),
                "{process}"
            );
        }
    }

    #[test]
    fn android_file_transfer_is_two_programs_with_one_name() {
        let names: Vec<_> = KNOWN
            .iter()
            .filter(|known| known.name == "Android File Transfer")
            .map(|known| (known.process, known.quit))
            .collect();

        assert_eq!(
            names,
            [
                ("Android File Transfer Agent", Quit::Kill),
                ("Android File Transfer", Quit::App)
            ]
        );
    }

    #[test]
    fn process_names_are_unique() {
        for (index, known) in KNOWN.iter().enumerate() {
            assert!(
                KNOWN[index + 1..]
                    .iter()
                    .all(|other| other.process != known.process),
                "{} is listed twice",
                known.process
            );
        }
    }

    #[test]
    fn the_android_file_transfer_agent_comes_first() {
        // It is the most common culprit and the one with its own error.
        assert_eq!(KNOWN[0].process, "Android File Transfer Agent");
        assert_eq!(KNOWN[0].quit, Quit::Kill);
    }

    #[test]
    fn only_the_camera_services_restart_themselves() {
        let restarting: Vec<_> = KNOWN
            .iter()
            .filter(|known| known.restarts)
            .map(|known| known.process)
            .collect();
        assert_eq!(restarting, CAMERA_PROCESSES);
        assert!(
            KNOWN
                .iter()
                .filter(|known| CAMERA_PROCESSES.contains(&known.process))
                .all(|known| known.name == CAMERA_SERVICES),
            "one holder to the user"
        );
    }

    #[test]
    fn names_are_safe_to_pass_to_applescript() {
        // The process name ends up inside an AppleScript string.
        for known in KNOWN {
            assert!(
                !known.process.contains(['"', '\\', '\n']),
                "{}",
                known.process
            );
        }
    }

    #[test]
    fn phone_makers_are_recognised_and_others_are_not() {
        assert_eq!(phone_vendor(0x18d1), Some("Google"));
        assert_eq!(phone_vendor(0x04e8), Some("Samsung"));
        assert_eq!(phone_vendor(0x05ac), None, "Apple makes more than phones");
        assert_eq!(phone_vendor(0x0000), None);
    }

    #[test]
    fn vendor_ids_are_unique() {
        for (index, (id, name)) in PHONE_VENDORS.iter().enumerate() {
            assert!(
                PHONE_VENDORS[index + 1..]
                    .iter()
                    .all(|(other, _)| other != id),
                "{name} ({id:#06x}) is listed twice"
            );
        }
    }
}
