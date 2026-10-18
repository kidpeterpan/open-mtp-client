//! What's new in each release, bundled with the app, newest first. Shown once
//! after an update (every release since the one last run, so a skipped
//! version is not missed) and always in Help's "What's new" tab.
//!
//! **A new version needs its notes here**: `every_version_of_the_app_has_its_notes`
//! fails when `Cargo.toml`'s version has no entry.

use internal::dto::Version;

/// One release: its version and what it brought, a line each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    pub version: &'static str,
    pub notes: &'static [&'static str],
}

/// Every release, newest first.
pub const RELEASES: &[Release] = &[Release {
    version: "0.1.0",
    notes: &[
        "Copy between your phone and your Mac in two panes, by button, ⌘C and ⌘V, or by dragging from one pane to the other.",
        "Moves that never lose a file: each original is removed only once its copy is checked.",
        "Transfers that finish: they skip what is already there, resume where they stopped, and end with a count of what arrived.",
        "Search the whole phone, import new photos into folders by day, month or year, and mirror a folder either way.",
        "Quick Look with Space, Get Info with ⌘I, and a filter as you type with ⌘F.",
        "Folders of thousands of photos stay quick: lists, grids with thumbnails, and a lasso to select.",
        "When the phone can't be reached, the app says which app is holding it and offers to quit it.",
        "A progress bar on the Dock icon, and a notification when transfers end while you are in another app.",
        "Your Mac stays awake while files copy, so a long transfer left overnight finishes.",
        "A real menu bar: every command in File, Edit, View, Go, Window and Help, with its shortcut, greyed out when it can't be used.",
        "The window opens where you left it, at the size you left it, and each pane keeps the column it was sorted by.",
        "Back and forward in each pane with ⌘[ and ⌘], the mouse's side buttons or the ‹ › buttons; going back up selects the folder you came from.",
        "Type the start of a name to jump to it, as in Finder, in any language.",
        "Help and Get Info say what your phone can do (copy or move by itself, resume past 4 GB, thumbnails), and Troubleshooting can time how fast it reads a folder.",
        "When macOS won't let the app into a folder, one click opens the right page of System Settings, and the folder opens when you come back.",
        "Files show their kind at a glance: a colour for photos, videos, music, documents, archives and Android apps, and a badge on tiles without a thumbnail.",
        "Undo with ⌘Z: the last rename, move or new folder, on the Mac or on the phone.",
        "Drag a column's edge in the list to make it wider or narrower; each pane remembers its widths, and a double-click on the edge puts them back.",
        "A transfer that won't fit is refused before it starts, saying how much it needs and how much is free, on the Mac or on the phone.",
        "Each pane opens where you left it: the Mac's folder, and each phone's own folder on each of its storages.",
        "A sound when transfers finish, if you want one (Settings): the Mac's Glass when all went, Basso when something didn't.",
        "Pause a transfer and resume it later: a download carries on from the part it had, an upload skips what the phone already has whole, and nothing else in the queue starts while it waits.",
        "Phone thumbnails are kept on your Mac: a folder of photos opened again, even after quitting, shows its pictures at once without asking the phone.",
        "Rename several items at once: replace text, add text, or give them one name and a number, with every new name shown before anything changes and one ⌘Z to undo the lot.",
        "Go to Folder (⇧⌘G) in either pane: type a path, pick from the folders it offers, and go.",
        "Click Kind in the list's header to sort by kind: folders, photos, videos, music, documents, archives, each grouped by type.",
        "Pick a kind beside the filter (Photos, Videos, Music…) to see only those files; ⌘A then selects just them.",
        "Import can free up the phone: tick \"Remove them from the phone once imported\" and each photo whose copy is checked is removed afterwards, saying how much room that made.",
        "New Folder with Selection (⌃⌘N): the selected items go into a new folder, whose name is ready to type; one ⌘Z puts them back.",
        "Copy Path (⌥⌘C): the selected items' paths as text, one per line.",
        "What fills the phone: in the phone search, click Size to list every file, largest first; Name and Modified order the results too.",
        "View › Calculate Folder Sizes: each folder's Size shows what is in it, counted in the background, so sorting by Size puts the biggest folders first.",
        "A command-line tool for scripts (open-mtp-client-cli): list, pull and push with the same checks as the window, plain or JSON.",
        "The Mac's disks in the sidebar show how full they are, as the phone's storages do.",
        "The phone's usual places in the sidebar: Camera, Screenshots, Downloads, WhatsApp media and Music, whichever the phone has, one click away.",
        "Spring-loaded folders: hold a drag over a folder for a moment (or press Space) and it opens, in either pane, so items go deep inside in one drag; let go elsewhere or press Esc and it closes again.",
        "Go › Recent Folders: the last ten folders shown on the Mac and on each phone, kept between launches; pick one to go back, or Clear Menu.",
        "Share… (right-click, or the File menu): send files through AirDrop, Messages, Mail or Notes, from the Mac or straight from the phone.",
        "Pick a date beside the filter (Today, Last 7 days, This year, Over a year ago…) in either pane or the phone search: this week's photos are one pick, then ⌘A. In a narrow pane the filter gets a row of its own.",
        "Saved mirrors: \"Save This Mirror\" keeps a pair of folders under the phone in the sidebar; a click compares them again, and Mirror runs it.",
        "The sidebar says how fast the phone's USB link is (USB 2 or USB 3), and a slow copy says when a USB 2 cable or port is why.",
        "Replace older: when a copy to the phone meets names already there, replace a file only with a newer one and leave the rest (also --replace-older in the command-line tool).",
        "Bigger or smaller tiles in Grid view: ⌘+ and ⌘− (View › Make Tiles Bigger / Smaller), remembered for each pane, with sharp thumbnails at every size.",
        "When macOS's own camera services grab the phone, the app frees it by itself and opens it straight away, instead of asking.",
        "Thumbnails the right way up: portrait photos show upright in the grid, from the Mac or the phone.",
        "Long names are cut in the middle, so the end and the extension stay in sight (IMG_2026…2133.jpg); hover to see the whole name.",
        "Downloading onto names your Mac already has asks first, as copying to the phone does: Skip, Keep both, Replace older or Replace (the old file goes to the Trash once the new one is safely in).",
        "Drop onto the sidebar: drag items onto a place, a pin, a phone storage or one of the phone's usual places to put them there (moved on the same disk, copied elsewhere; ⌥ and ⌘ turn that round), and hold a moment to open it in its pane. Internal storage to SD card in one drag.",
        "Finder's keys for long lists: Home and End (or ⌥↑ and ⌥↓) select the first or last item, Page Up and Page Down move a screen, each with ⇧ to select the run; ⇧⌘. shows hidden files; ⇧⌘H, ⇧⌘D and ⌥⌘L take the Mac pane to Home, the Desktop and Downloads (also in the Go menu).",
        "A transfer cut off by a pulled cable or a locked phone waits instead of failing: plug the phone back in and unlock it (or Reconnect) and it carries on by itself from where it stopped, without starting over.",
        "Edit › Select Not in Other Pane (or right-click): selects the files the other pane's folder doesn't have yet, so only the new photos get copied; Select Also in Other Pane selects what is already there, to free up the phone.",
        "Details on a finished download, move or import: what became of every file (copied and checked, already there, replaced, skipped, failed and why, the original removed or kept), with Save Report (a spreadsheet file beside the copies) and Copy.",
        "A finished transfer says how long it took and how fast it went (“Took 1 min 11 s · 29.6 MB/s on average”), and when a slow USB 2 link was why.",
        "Quitting (⌘Q, the menu, or the window's close button) while a transfer runs asks first, and Stop and Quit stops it safely, so a download carries on from where it stopped next time.",
        "Copying to a disk that can't be written (an NTFS drive, a locked SD card) is refused before anything moves, and on a FAT32 disk each file of 4 GB or more is named and left out while the rest copy, instead of failing after gigabytes.",
        "Details on a finished upload too: what became of every file sent to the phone (sent, read back, already there, kept as the phone had it, replaced, failed and why) and, for a move, whether each original went to the Trash; Copy puts it on the clipboard.",
        "The command-line tool grows: info, mkdir -p, rm (asks first, --yes in scripts), mv, pull of several paths, --move both ways, progress on stderr, and ctrl-c that stops a copy safely (exit 130).",
        "The command-line tool now comes inside the app: Help › About shows where, with a line to copy that puts it in your Terminal's path.",
        "Get Info on a folder or a storage shows what fills it, by kind, with a bar; a click on Videos or Photos finds them, the biggest first.",
        "Use Groups (⌃⌘0), as in Finder: each pane can show its files under Today, Yesterday, each month and year (or by kind or size), and a click on a header selects that group, so “the photos from March” is one click.",
        "Deleting from the phone can keep a copy in the Mac's Trash first (Settings, off unless you turn it on, up to 2 GB a delete): only what was copied whole is deleted.",
        "Edit a phone file in its Mac app: right-click › Edit on Mac. Once you save it, Send to Phone puts it back (beside the phone's copy, if that changed meanwhile), and edits not yet sent are still there after a restart.",
        "Free Up Space…, under the phone in the sidebar: copies of the same file (read in full to be sure, the oldest kept), photos already imported and still on this Mac, and a way to the biggest files.",
        "On an Android phone, Android/data and Android/obb say why they look empty: Android keeps apps' own folders from computers.",
        "Check copies byte for byte: select files in one pane and choose Edit › Check Contents Against Other Pane. Each phone file is read and compared with the Mac's, nothing is changed, and Details says which are identical, which differ and from which byte.",
        "When your phone won't open because something has it, the app asks macOS who and names that program, instead of every app that happens to run; it now knows Garmin Express, MightyText, Commander One, MacDroid, DaVinci Resolve and Nikon Transfer too.",
        "Transfers not finished when the app last quit or crashed are offered again (a phone's when that phone opens): Continue carries on from where they stopped, Discard forgets them.",
    ],
}];

/// The version this app is.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml's version is major.minor.patch")
}

/// The releases to show when version `current` starts after `last_seen` ran:
/// the ones newer than `last_seen`, up to `current`. Nothing on a first run
/// (there is no "new" yet), and nothing when the version is the same or
/// older.
pub fn after_update(last_seen: Option<Version>, current: Version) -> Vec<Release> {
    releases_between(RELEASES, last_seen, current)
}

/// `after_update` over the list `releases`.
fn releases_between(
    releases: &[Release],
    last_seen: Option<Version>,
    current: Version,
) -> Vec<Release> {
    let Some(last_seen) = last_seen else {
        return Vec::new();
    };
    releases
        .iter()
        .filter(|release| {
            Version::parse(release.version)
                .is_some_and(|version| version > last_seen && version <= current)
        })
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn every_version_of_the_app_has_its_notes() {
        assert_eq!(
            RELEASES[0].version,
            env!("CARGO_PKG_VERSION"),
            "the newest notes are this version's: write them in whats_new.rs"
        );
        let versions: Vec<Version> = RELEASES.iter().map(|release| v(release.version)).collect();
        assert!(
            versions.windows(2).all(|pair| pair[0] > pair[1]),
            "newest first, each once: {versions:?}"
        );
        assert!(RELEASES.iter().all(|release| !release.notes.is_empty()));
    }

    #[test]
    fn after_an_update_every_release_since_the_last_run_is_shown() {
        // Releases as they might be later on.
        let shown = |last: Option<&str>, now: &str| -> Vec<&str> {
            let releases = [
                Release {
                    version: "0.3.0",
                    notes: &["c"],
                },
                Release {
                    version: "0.2.0",
                    notes: &["b"],
                },
                Release {
                    version: "0.1.0",
                    notes: &["a"],
                },
            ];
            let last = last.map(v);
            releases
                .iter()
                .filter(|release| releases_between(&releases, last, v(now)).contains(release))
                .map(|release| release.version)
                .collect()
        };

        assert_eq!(
            shown(Some("0.1.0"), "0.3.0"),
            ["0.3.0", "0.2.0"],
            "a skipped version too"
        );
        assert_eq!(shown(Some("0.2.0"), "0.3.0"), ["0.3.0"]);
        assert_eq!(
            shown(Some("0.1.0"), "0.2.0"),
            ["0.2.0"],
            "notes written ahead of this version are not shown yet"
        );
        assert!(
            shown(None, "0.3.0").is_empty(),
            "a first run has nothing new"
        );
        assert!(
            shown(Some("0.3.0"), "0.3.0").is_empty(),
            "the same version again"
        );
        assert!(shown(Some("0.3.0"), "0.2.0").is_empty(), "an older version");
    }

    #[test]
    fn this_releases_notes_show_after_an_update_from_an_older_one() {
        let shown = after_update(Some(v("0.0.9")), current());
        assert_eq!(
            shown.first().map(|release| release.version),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(after_update(Some(current()), current()).is_empty());
        assert!(after_update(None, current()).is_empty());
    }
}
