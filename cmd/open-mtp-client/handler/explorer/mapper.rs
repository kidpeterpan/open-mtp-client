use chrono::NaiveDate;
use internal::dto;

use super::response::{
    Badge, BadgeKind, CrumbResponse, EntryResponse, NoticeResponse, PasteResponse,
};
use crate::handler::error;
use crate::handler::format::{format_bytes, format_count, format_timestamp};

pub const MISSING: &str = "—";

/// A thumbnail's pixels as something the grid can draw.
pub fn to_picture(thumbnail: dto::Thumbnail) -> iced::widget::image::Handle {
    iced::widget::image::Handle::from_rgba(thumbnail.width, thumbnail.height, thumbnail.rgba)
}

/// What Finder calls a new folder.
pub const NEW_FOLDER_NAME: &str = "untitled folder";

/// Under a phone delete's question: whether a copy goes to the Trash first.
pub fn delete_note(keep: super::handler::Keep) -> String {
    use super::handler::Keep;
    let gone = "It is deleted from your phone for good. It can't be undone.";
    match keep {
        Keep::No => "This deletes it from your phone for good. It can't be undone.".to_owned(),
        Keep::Counting => "Counting what will be copied to the Trash first…".to_owned(),
        Keep::Copy(bytes) => format!(
            "A copy ({}) goes to the Mac's Trash first, then it is deleted from the phone. Anything that can't be copied whole stays on the phone.",
            crate::handler::format::format_bytes(bytes)
        ),
        Keep::TooMuch(bytes) => format!(
            "This is {}, over the {} a delete copies to the Trash first. {gone}",
            crate::handler::format::format_bytes(bytes),
            crate::handler::format::format_bytes(dto::KEEP_MOST)
        ),
        Keep::Unknown => format!("The phone doesn't give every size, so no copy is kept. {gone}"),
    }
}

/// Said in an Android phone's `Android/data` or `Android/obb` when it looks
/// empty or can't be read: Android keeps them from computers.
pub const APP_FOLDERS_NOTE: &str =
    "Android keeps apps' own folders from computers: what is here can be seen only on the phone.";

/// The title of a refused read in one of those folders.
pub const APP_FOLDERS_REFUSED: &str = "Android doesn't let computers in here";

/// `path` is `Android/data` or `Android/obb` on a phone, or inside one,
/// whatever its case: the folders Android keeps from computers (Android 11
/// and later). Works on the path alone, so it costs no request.
pub fn in_app_folders(path: &str) -> bool {
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('/').filter(|part| !part.is_empty()).collect();
    matches!(parts.as_slice(), ["android", "data" | "obb", ..])
}
/// A path holding a line break, a tab or another control character (C0, DEL
/// or C1), a Unicode line or paragraph separator, or a character that turns
/// the direction of text (which can make a pasted path read as another):
/// anything that would change what a pasted path list says.
pub fn has_control(path: &str) -> bool {
    crate::handler::format::has_control(path)
}

/// Why a path wasn't copied: the path, with those characters shown escaped.
pub fn odd_name_hint(path: &str) -> String {
    let shown = crate::handler::format::escape_odd(path);
    format!(
        "“{shown}” has a line break or another control character in a name, which would change what is pasted. Rename it first."
    )
}

/// A folder's Size cell while it is being counted.
pub const COUNTING: &str = "Counting…";

/// A folder's Size cell once counted: what its files hold, with a "+" when
/// some of it could not be counted (sizes the phone doesn't give, folders
/// macOS keeps private, or more than a count looks at).
pub fn folder_size(tally: &dto::Tally) -> String {
    let some_missing = tally.unknown_sizes > 0 || tally.unreadable > 0 || tally.incomplete;
    format!(
        "{}{}",
        format_bytes(tally.bytes),
        if some_missing { "+" } else { "" }
    )
}

/// What the banner says once paths are on the clipboard.
pub fn copied_paths(names: &[String]) -> String {
    match names {
        [one] => format!("Copied the path of “{one}”"),
        many => format!("Copied {} paths", format_count(many.len())),
    }
}

/// What a folder made around the selection is called first, as in Finder.
pub const FOLDER_WITH_ITEMS: &str = "New Folder With Items";

/// `base`, or `base (n)` for the first n that no name in `taken` has,
/// ignoring case as phones and the Mac do.
pub fn free_folder_name<'a>(base: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    let is_taken = |name: &str| {
        let wanted = name.to_lowercase();
        taken.clone().any(|other| other.to_lowercase() == wanted)
    };
    std::iter::once(base.to_owned())
        .chain((1..).map(|n| format!("{base} ({n})")))
        .find(|name| !is_taken(name))
        .unwrap_or_else(|| base.to_owned())
}

/// Characters before the extension, so a rename selects "IMG_001" in
/// "IMG_001.jpg". Dot-files and names without an extension select it all.
pub fn stem_len(name: &str) -> usize {
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[..dot].chars().count(),
        _ => name.chars().count(),
    }
}

/// "Delete “IMG_001.jpg”?" or "Delete 3 items?"; "Move … to the Trash?" on the Mac.
pub fn delete_title(names: &[String], permanently: bool) -> String {
    let what = match names {
        [one] => format!("“{one}”"),
        many => format!("{} items", many.len()),
    };
    if permanently {
        format!("Delete {what}?")
    } else {
        format!("Move {what} to the Trash?")
    }
}

/// What the banner says while items are dragged: where they would land, and
/// what ⌥ does.
pub fn drag_hint(count: usize, target: Option<&str>, copy: bool, other_pane: bool) -> String {
    let what = items(count);
    let place = if other_pane {
        "a folder or the other pane"
    } else {
        "a folder"
    };
    match (target, copy) {
        (Some(folder), true) => format!("Copy {what} into “{folder}”"),
        (Some(folder), false) => format!("Move {what} into “{folder}” · hold ⌥ to copy"),
        (None, _) => format!("Dragging {what}: drop on {place} · ⌥ copies · Esc cancels"),
    }
}

/// What dropping the other pane's drag here does: copy into the folder row
/// it is over, or into the folder shown; with ⌘, move (as Finder does
/// between disks).
pub fn incoming_hint(count: usize, folder: Option<&str>, moving: bool) -> String {
    let what = items(count);
    let place = match folder {
        Some(folder) => format!("into “{folder}”"),
        None => "here".to_owned(),
    };
    if moving {
        format!("Move {what} {place}")
    } else {
        format!("Copy {what} {place} · hold ⌘ to move")
    }
}

/// The last part of a path: the name of the folder it points at.
pub fn folder_name(path: &str) -> String {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => "the top folder".to_owned(),
    }
}

/// What selecting against the other pane's folder found: "Selected 1,000
/// of 7,000: not in “Backup”", or that nothing was.
pub fn against_notice(
    want: super::compare::Against,
    picked: usize,
    shown: usize,
    other: &str,
) -> NoticeResponse {
    use super::compare::Against;
    let title = match (want, picked) {
        (Against::Missing, 0) => format!("Everything here is in “{other}” already"),
        (Against::Same, 0) => format!("Nothing here is in “{other}” as it is here"),
        (Against::Missing, n) => format!(
            "Selected {} of {}: not in “{other}”",
            format_count(n),
            format_count(shown)
        ),
        (Against::Same, n) => format!(
            "Selected {} of {}: also in “{other}”",
            format_count(n),
            format_count(shown)
        ),
    };
    NoticeResponse {
        title,
        details: Vec::new(),
        problem: false,
    }
}

/// The most failures a notice lists by name; the rest are counted.
const FAILURES_LISTED: usize = 3;

fn items(count: usize) -> String {
    match count {
        1 => "1 item".to_owned(),
        n => format!("{} items", format_count(n)),
    }
}

/// What a finished paste tells the user, and which names to select.
pub fn to_paste_response(summary: &dto::PasteSummary) -> PasteResponse {
    let (past, verb) = match summary.mode {
        dto::Paste::Copy => ("Copied", "copy"),
        dto::Paste::Move => ("Moved", "move"),
    };
    let failed = summary.failures.len();
    let mut details = Vec::new();

    let title = match (summary.done, failed) {
        (0, 0) => match summary.mode {
            dto::Paste::Copy => "Nothing to copy".to_owned(),
            dto::Paste::Move => "Already in this folder".to_owned(),
        },
        (1, 0) if summary.renamed == 1 => {
            // Say what it is called now: it is not what was copied.
            format!("{past} as “{}”", summary.names[0])
        }
        (1, 0) => format!("{past} “{}”", summary.names[0]),
        (done, 0) => format!("{past} {}", items(done)),
        (0, 1) => format!("Couldn’t {verb} “{}”", summary.failures[0].label),
        (0, many) => format!("Couldn’t {verb} {}", items(many)),
        (done, failed) => format!("{past} {done} of {}", items(done + failed)),
    };

    // A lone renamed item is already named in the title.
    if summary.renamed > 0 && !(summary.done == 1 && failed == 0) {
        details.push(match summary.renamed {
            1 => "1 item was renamed because its name was taken.".to_owned(),
            n => format!(
                "{} items were renamed because their names were taken.",
                format_count(n)
            ),
        });
    }
    for failure in summary.failures.iter().take(FAILURES_LISTED) {
        let reason = error::to_error_response(failure.error.clone()).title;
        details.push(format!("“{}”: {reason}", failure.label));
    }
    if failed > FAILURES_LISTED {
        details.push(format!(
            "and {} more",
            format_count(failed - FAILURES_LISTED)
        ));
    }

    PasteResponse {
        select: summary.names.clone(),
        notice: NoticeResponse {
            title,
            details,
            problem: failed > 0,
        },
        undo: None,
    }
}

/// A paste that could not even start: the reason, as a banner.
pub fn to_paste_failure(error: &error::ErrorResponse) -> PasteResponse {
    PasteResponse {
        select: Vec::new(),
        notice: NoticeResponse {
            title: error.title.clone(),
            details: vec![error.hint.clone()],
            problem: true,
        },
        undo: None,
    }
}

/// `today` makes recent dates relative ("Today, 12:33").
pub fn to_entry_response(entry: &dto::Entry, today: NaiveDate) -> EntryResponse {
    EntryResponse {
        name: entry.name.clone(),
        is_folder: entry.is_folder,
        kind: kind(entry),
        size: if entry.is_folder {
            MISSING.to_owned()
        } else {
            format_bytes(entry.size_bytes)
        },
        modified: entry
            .modified
            .as_ref()
            .map_or_else(|| MISSING.to_owned(), |time| format_timestamp(time, today)),
        badge: badge(entry),
    }
}

/// The longest label a badge shows: a longer extension is cut.
const BADGE_LABEL_MOST: usize = 4;

/// A file's badge: its kind from the extension (the kinds search knows,
/// with Android apps told apart from archives) and the extension as its
/// label. `None` for a folder.
pub fn badge(entry: &dto::Entry) -> Option<Badge> {
    if entry.is_folder {
        return None;
    }
    let extension = match entry.name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension,
        _ => "",
    };
    let lower = extension.to_ascii_lowercase();
    let kind = match (lower.as_str(), dto::FileKind::of(&entry.name)) {
        ("apk" | "apks" | "xapk", _) => BadgeKind::App,
        (_, Some(dto::FileKind::Photo)) => BadgeKind::Photo,
        (_, Some(dto::FileKind::Video)) => BadgeKind::Video,
        (_, Some(dto::FileKind::Music)) => BadgeKind::Music,
        (_, Some(dto::FileKind::Document)) => BadgeKind::Document,
        (_, Some(dto::FileKind::Archive)) => BadgeKind::Archive,
        (_, None) => BadgeKind::Other,
    };
    Some(Badge {
        kind,
        label: extension
            .chars()
            .take(BADGE_LABEL_MOST)
            .collect::<String>()
            .to_uppercase(),
    })
}

/// Finder-style kind column: "Folder", "JPG file", or "File".
pub fn kind(entry: &dto::Entry) -> String {
    if entry.is_folder {
        return "Folder".to_owned();
    }
    match entry.name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            format!("{} file", extension.to_uppercase())
        }
        _ => "File".to_owned(),
    }
}

/// Next sort after a header click. The active column flips its order; a new
/// column starts A→Z for names and kinds and largest/newest first for size
/// and date, as Finder does.
pub fn next_sort(current: dto::Sort, key: dto::SortKey) -> dto::Sort {
    let descending = if current.key == key {
        !current.descending
    } else {
        !matches!(key, dto::SortKey::Name | dto::SortKey::Kind)
    };
    dto::Sort {
        key,
        descending,
        ..current
    }
}

/// Column title with an arrow on the active sort column.
pub fn header_label(title: &str, key: dto::SortKey, sort: dto::Sort) -> String {
    match (sort.key == key, sort.descending) {
        (false, _) => title.to_owned(),
        (true, false) => format!("{title} ▲"),
        (true, true) => format!("{title} ▼"),
    }
}

/// Status bar line, leaving out empty groups: "5 items · 2 folders, 3 files
/// · 4.2 MB", "3 files · 53.5 MB", "2 folders"; or the selection count once
/// something is selected.
/// The footer while a filter hides part of the folder.
pub fn filtered_status(shown: usize, total: usize, selected: usize) -> String {
    let shown_of = format!("{} of {} shown", format_count(shown), format_count(total));
    match selected {
        0 => shown_of,
        n => format!("{} selected · {shown_of}", format_count(n)),
    }
}

/// What the selection holds: how many of how many, and the size of its files.
/// A folder's size would mean reading all of it, so folders are only counted
/// and ⌘I offers to measure them.
pub fn selected_status(selected: usize, total: usize, file_bytes: u64, folders: usize) -> String {
    let some = format!(
        "{} of {} selected",
        format_count(selected),
        format_count(total)
    );
    match folders {
        0 => format!("{some} · {}", format_bytes(file_bytes)),
        n if n == selected => format!("{some} · ⌘I to count what is in them"),
        n => format!(
            "{some} · {} + {} · ⌘I to count",
            format_bytes(file_bytes),
            plural(n, "folder")
        ),
    }
}

pub fn status(entries: &[dto::Entry]) -> String {
    let total = entries.len();

    let folders = entries.iter().filter(|entry| entry.is_folder).count();
    let files = total - folders;
    let bytes: u64 = entries.iter().map(|entry| entry.size_bytes).sum();

    match (folders, files) {
        (0, 0) => "No items".to_owned(),
        (_, 0) => plural(folders, "folder"),
        (0, _) => format!("{} · {}", plural(files, "file"), format_bytes(bytes)),
        _ => format!(
            "{} · {}, {} · {}",
            plural(total, "item"),
            plural(folders, "folder"),
            plural(files, "file"),
            format_bytes(bytes)
        ),
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Long paths keep the first crumb and the last two, with "…" standing in
/// for (and opening) the folder just above them, so the trail fits a pane.
pub fn compact_breadcrumbs(crumbs: Vec<CrumbResponse>) -> Vec<CrumbResponse> {
    const MAX: usize = 4;
    if crumbs.len() <= MAX {
        return crumbs;
    }
    let tail = crumbs.len() - 2;
    let mut compact = vec![crumbs[0].clone()];
    compact.push(CrumbResponse {
        label: "…".to_owned(),
        path: crumbs[tail - 1].path.clone(),
    });
    compact.extend_from_slice(&crumbs[tail..]);
    compact
}

/// One crumb per folder on the way to `path`. The first is the volume's
/// root, or a single "Home" crumb when `path` is inside `home`, as Finder does.
pub fn breadcrumbs(volume_name: &str, home: Option<&str>, path: &str) -> Vec<CrumbResponse> {
    let inside_home = home.filter(|home| {
        path == *home
            || path
                .strip_prefix(home)
                .is_some_and(|rest| rest.starts_with('/'))
    });
    let (first, start, rest) = match inside_home {
        Some(home) => ("Home", home, &path[home.len()..]),
        None => (volume_name, "/", path),
    };

    let mut crumbs = vec![CrumbResponse {
        label: first.to_owned(),
        path: start.to_owned(),
    }];
    let mut walked = start.trim_end_matches('/').to_owned();
    for segment in rest.split('/').filter(|segment| !segment.is_empty()) {
        walked.push('/');
        walked.push_str(segment);
        crumbs.push(CrumbResponse {
            label: segment.to_owned(),
            path: walked.clone(),
        });
    }
    crumbs
}

pub fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// `None` at the storage root.
pub fn parent_path(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    match path.rsplit_once('/') {
        Some(("", _)) | None => Some("/".to_owned()),
        Some((parent, _)) => Some(parent.to_owned()),
    }
}

/// Whether `name` starts with `typed`, letters in either case.
pub fn starts_with_typed(name: &str, typed: &str) -> bool {
    let mut name = name.chars().flat_map(char::to_lowercase);
    typed
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| name.next() == Some(wanted))
}

/// The folder in `ancestor` that leads down to `path`, when `path` is below
/// it: what to select on going back up, as Finder selects the folder you
/// came out of.
pub fn child_toward(ancestor: &str, path: &str) -> Option<String> {
    let rest = if ancestor == "/" {
        path.strip_prefix('/')?
    } else {
        path.strip_prefix(ancestor)?.strip_prefix('/')?
    };
    rest.split('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn androids_app_folders_are_known_by_their_path_in_any_case() {
        for path in [
            "/Android/data",
            "/Android/obb",
            "/android/DATA/",
            "/Android/data/com.example.app/files",
        ] {
            assert!(in_app_folders(path), "{path}");
        }
        for path in [
            "/",
            "/Android",
            "/Android/media",
            "/Android/database",
            "/DCIM/Android/data",
            "Android",
        ] {
            assert!(!in_app_folders(path), "{path}");
        }
    }

    #[test]
    fn selecting_against_the_other_folder_says_how_many_of_how_many_or_that_none() {
        use super::super::compare::Against;
        let title = |want, picked, shown| against_notice(want, picked, shown, "Backup").title;
        assert_eq!(
            title(Against::Missing, 1_000, 7_000),
            "Selected 1,000 of 7,000: not in “Backup”"
        );
        assert_eq!(
            title(Against::Same, 3, 9),
            "Selected 3 of 9: also in “Backup”"
        );
        assert_eq!(
            title(Against::Missing, 0, 9),
            "Everything here is in “Backup” already"
        );
        assert_eq!(
            title(Against::Same, 0, 9),
            "Nothing here is in “Backup” as it is here"
        );
        assert!(!against_notice(Against::Same, 0, 9, "x").problem);
    }

    #[test]
    fn a_file_is_badged_by_its_extension_and_an_app_is_not_an_archive() {
        let badge_of = |name: &str| badge(&file(name)).map(|badge| (badge.kind, badge.label));
        assert_eq!(
            badge_of("IMG_2041.JPG"),
            Some((BadgeKind::Photo, "JPG".into()))
        );
        assert_eq!(badge_of("trip.mov"), Some((BadgeKind::Video, "MOV".into())));
        assert_eq!(
            badge_of("song.flac"),
            Some((BadgeKind::Music, "FLAC".into()))
        );
        assert_eq!(
            badge_of("notes.pdf"),
            Some((BadgeKind::Document, "PDF".into()))
        );
        assert_eq!(
            badge_of("backup.zip"),
            Some((BadgeKind::Archive, "ZIP".into()))
        );
        assert_eq!(
            badge_of("WhatsApp.apk"),
            Some((BadgeKind::App, "APK".into()))
        );
        assert_eq!(badge_of("game.xapk"), Some((BadgeKind::App, "XAPK".into())));
        assert_eq!(badge_of("data.bin"), Some((BadgeKind::Other, "BIN".into())));
        assert_eq!(
            badge_of("model.safetensors"),
            Some((BadgeKind::Other, "SAFE".into())),
            "a long extension is cut"
        );
        assert_eq!(badge_of("README"), Some((BadgeKind::Other, String::new())));
        assert_eq!(
            badge_of(".nomedia"),
            Some((BadgeKind::Other, String::new()))
        );
        assert_eq!(badge(&crate::handler::testing::folder("Camera")), None);
        assert_eq!(
            to_entry_response_now(&file("a.mp3"))
                .badge
                .map(|badge| badge.kind),
            Some(BadgeKind::Music)
        );
        assert_eq!(BadgeKind::App.word(), "Android app");
    }

    #[test]
    fn typing_matches_the_start_of_a_name_in_either_case() {
        assert!(starts_with_typed("IMG_2041.jpg", "img_2"));
        assert!(starts_with_typed("photos", "PHO"));
        assert!(starts_with_typed("ภาพถ่าย", "ภา"), "Thai");
        assert!(starts_with_typed("Ärger.txt", "är"));
        assert!(!starts_with_typed("IMG", "img_"), "longer than the name");
        assert!(!starts_with_typed("my photos", "photos"), "the start only");
        assert!(starts_with_typed("anything", ""));
    }

    #[test]
    fn the_folder_that_leads_down_is_found_only_below() {
        assert_eq!(child_toward("/", "/DCIM/Camera").as_deref(), Some("DCIM"));
        assert_eq!(
            child_toward("/DCIM", "/DCIM/Camera/2026").as_deref(),
            Some("Camera")
        );
        assert_eq!(child_toward("/DCIM", "/DCIM"), None, "the same folder");
        assert_eq!(
            child_toward("/DCIM", "/DCIMx/a"),
            None,
            "a name that only starts alike"
        );
        assert_eq!(
            child_toward("/DCIM/Camera", "/DCIM"),
            None,
            "above, not below"
        );
        assert_eq!(child_toward("/", "/"), None);
    }

    fn file(name: &str) -> dto::Entry {
        dto::Entry {
            id: 0,
            name: name.to_owned(),
            is_folder: false,
            size_bytes: 2_500_000,
            modified: None,
        }
    }

    fn to_entry_response_now(entry: &dto::Entry) -> EntryResponse {
        to_entry_response(entry, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap())
    }

    #[test]
    fn modified_dates_are_relative_to_today() {
        let entry = dto::Entry {
            modified: Some(dto::Timestamp {
                year: 2026,
                month: 10,
                day: 16,
                hour: 12,
                minute: 33,
            }),
            ..file("a.jpg")
        };

        assert_eq!(to_entry_response_now(&entry).modified, "Today, 12:33");
    }

    fn labels_and_paths(crumbs: &[CrumbResponse]) -> (Vec<&str>, Vec<&str>) {
        (
            crumbs.iter().map(|c| c.label.as_str()).collect(),
            crumbs.iter().map(|c| c.path.as_str()).collect(),
        )
    }

    #[test]
    fn inside_home_the_trail_starts_at_home() {
        let crumbs = breadcrumbs("Mac", Some("/Users/me"), "/Users/me/dev-tools");

        let (labels, paths) = labels_and_paths(&crumbs);
        assert_eq!(labels, ["Home", "dev-tools"]);
        assert_eq!(paths, ["/Users/me", "/Users/me/dev-tools"]);
    }

    #[test]
    fn home_itself_is_a_single_crumb() {
        let crumbs = breadcrumbs("Mac", Some("/Users/me"), "/Users/me");

        assert_eq!(labels_and_paths(&crumbs).0, ["Home"]);
    }

    #[test]
    fn above_home_the_trail_starts_at_the_volume() {
        let crumbs = breadcrumbs("Mac", Some("/Users/me"), "/Users");

        assert_eq!(labels_and_paths(&crumbs).0, ["Mac", "Users"]);
    }

    #[test]
    fn a_sibling_with_a_longer_name_is_not_inside_home() {
        let crumbs = breadcrumbs("Mac", Some("/Users/me"), "/Users/meg");

        assert_eq!(labels_and_paths(&crumbs).0, ["Mac", "Users", "meg"]);
    }

    #[test]
    fn short_paths_keep_every_crumb() {
        let crumbs = breadcrumbs("Mac", None, "/Users/me/Music");

        assert_eq!(compact_breadcrumbs(crumbs.clone()), crumbs);
    }

    #[test]
    fn long_paths_collapse_the_middle_into_a_link_to_it() {
        let crumbs = compact_breadcrumbs(breadcrumbs("Mac", None, "/Users/me/Music/Albums/2026"));

        let labels: Vec<_> = crumbs.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Mac", "…", "Albums", "2026"]);
        assert_eq!(crumbs[1].path, "/Users/me/Music");
    }

    #[test]
    fn kind_names_the_extension() {
        assert_eq!(to_entry_response_now(&file("IMG_001.jpg")).kind, "JPG file");
        assert_eq!(to_entry_response_now(&file("README")).kind, "File");
        assert_eq!(to_entry_response_now(&file(".nomedia")).kind, "File");
    }

    #[test]
    fn folders_have_no_size() {
        let folder = dto::Entry {
            is_folder: true,
            ..file("DCIM")
        };

        let response = to_entry_response_now(&folder);

        assert_eq!(response.kind, "Folder");
        assert_eq!(response.size, "—");
    }

    #[test]
    fn files_show_size_and_missing_dates() {
        let response = to_entry_response_now(&file("song.mp3"));

        assert_eq!(response.size, "2.5 MB");
        assert_eq!(response.modified, "—");
    }

    #[test]
    fn breadcrumbs_start_at_the_storage() {
        let crumbs = breadcrumbs("Internal storage", None, "/DCIM/Camera");

        let labels: Vec<_> = crumbs.iter().map(|c| c.label.as_str()).collect();
        let paths: Vec<_> = crumbs.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(labels, ["Internal storage", "DCIM", "Camera"]);
        assert_eq!(paths, ["/", "/DCIM", "/DCIM/Camera"]);
    }

    #[test]
    fn paths_join_and_split() {
        assert_eq!(child_path("/", "DCIM"), "/DCIM");
        assert_eq!(child_path("/DCIM", "Camera"), "/DCIM/Camera");
        assert_eq!(parent_path("/DCIM/Camera").as_deref(), Some("/DCIM"));
        assert_eq!(parent_path("/DCIM").as_deref(), Some("/"));
        assert_eq!(parent_path("/"), None);
    }

    fn sort(key: dto::SortKey, descending: bool) -> dto::Sort {
        dto::Sort {
            key,
            descending,
            folders_first: true,
        }
    }

    #[test]
    fn clicking_the_active_column_flips_its_order() {
        let next = next_sort(sort(dto::SortKey::Name, false), dto::SortKey::Name);

        assert_eq!(next, sort(dto::SortKey::Name, true));
    }

    #[test]
    fn a_new_name_column_starts_ascending() {
        let next = next_sort(sort(dto::SortKey::Size, true), dto::SortKey::Name);

        assert_eq!(next, sort(dto::SortKey::Name, false));
    }

    #[test]
    fn a_new_kind_column_starts_from_folders_and_photos() {
        let current = sort(dto::SortKey::Size, true);

        assert_eq!(
            next_sort(current, dto::SortKey::Kind),
            sort(dto::SortKey::Kind, false)
        );
        assert_eq!(
            next_sort(sort(dto::SortKey::Kind, false), dto::SortKey::Kind),
            sort(dto::SortKey::Kind, true),
            "again: the other way"
        );
    }

    #[test]
    fn new_size_and_date_columns_start_largest_and_newest_first() {
        let current = sort(dto::SortKey::Name, false);

        assert_eq!(
            next_sort(current, dto::SortKey::Size),
            sort(dto::SortKey::Size, true)
        );
        assert_eq!(
            next_sort(current, dto::SortKey::Modified),
            sort(dto::SortKey::Modified, true)
        );
    }

    #[test]
    fn changing_column_keeps_folders_first() {
        let current = dto::Sort {
            folders_first: false,
            ..sort(dto::SortKey::Name, false)
        };

        assert!(!next_sort(current, dto::SortKey::Size).folders_first);
    }

    #[test]
    fn header_arrow_marks_the_active_column_and_direction() {
        let by_size_desc = sort(dto::SortKey::Size, true);

        assert_eq!(
            header_label("Size", dto::SortKey::Size, by_size_desc),
            "Size ▼"
        );
        assert_eq!(
            header_label("Name", dto::SortKey::Name, by_size_desc),
            "Name"
        );
        assert_eq!(
            header_label("Name", dto::SortKey::Name, sort(dto::SortKey::Name, false)),
            "Name ▲"
        );
    }

    #[test]
    fn status_counts_folders_files_and_size() {
        let entries = [
            dto::Entry {
                is_folder: true,
                size_bytes: 0,
                ..file("DCIM")
            },
            file("a.mp3"),
            file("b.mp3"),
        ];

        assert_eq!(status(&entries), "3 items · 1 folder, 2 files · 5.0 MB");
    }

    #[test]
    fn status_with_only_folders_has_no_size() {
        let entries = [dto::Entry {
            is_folder: true,
            size_bytes: 0,
            ..file("DCIM")
        }];

        assert_eq!(status(&entries), "1 folder");
    }

    #[test]
    fn status_with_only_files_skips_the_item_total() {
        assert_eq!(status(&[file("a.mp3"), file("b.mp3")]), "2 files · 5.0 MB");
    }

    #[test]
    fn status_of_an_empty_folder() {
        assert_eq!(status(&[]), "No items");
    }

    #[test]
    fn status_says_how_big_the_selected_files_are_and_offers_to_count_folders() {
        assert_eq!(
            selected_status(2, 9, 12_400_000, 0),
            "2 of 9 selected · 12.4 MB"
        );
        assert_eq!(
            selected_status(3, 9, 12_400_000, 1),
            "3 of 9 selected · 12.4 MB + 1 folder · ⌘I to count"
        );
        assert_eq!(
            selected_status(2, 5_000, 0, 2),
            "2 of 5,000 selected · ⌘I to count what is in them"
        );
    }

    #[test]
    fn stem_stops_before_the_extension() {
        assert_eq!(stem_len("IMG_001.jpg"), 7);
        assert_eq!(stem_len("archive.tar.gz"), 11);
        assert_eq!(stem_len("README"), 6);
        assert_eq!(stem_len(".nomedia"), 8);
        assert_eq!(stem_len("ภาพ.jpg"), 3);
    }

    #[test]
    fn delete_title_names_one_item_and_counts_many() {
        let one = ["IMG_001.jpg".to_owned()];
        let many = ["a".to_owned(), "b".to_owned(), "c".to_owned()];

        assert_eq!(delete_title(&one, true), "Delete “IMG_001.jpg”?");
        assert_eq!(delete_title(&many, true), "Delete 3 items?");
        assert_eq!(
            delete_title(&one, false),
            "Move “IMG_001.jpg” to the Trash?"
        );
    }

    fn summary(
        mode: dto::Paste,
        names: &[&str],
        renamed: usize,
        failed: &[&str],
    ) -> dto::PasteSummary {
        dto::PasteSummary {
            mode,
            done: names.len(),
            renamed,
            into: "/Music".into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            failures: failed
                .iter()
                .map(|label| dto::FileFailure {
                    label: (*label).to_owned(),
                    error: dto::Error::ReadOnly,
                })
                .collect(),
        }
    }

    #[test]
    fn a_paste_says_what_was_copied_or_moved_and_selects_it() {
        let one = to_paste_response(&summary(dto::Paste::Copy, &["a.jpg"], 0, &[]));
        assert_eq!(one.notice.title, "Copied “a.jpg”");
        assert_eq!(one.select, ["a.jpg"]);
        assert!(!one.notice.problem && one.notice.details.is_empty());

        let many = to_paste_response(&summary(dto::Paste::Move, &["a", "b", "c"], 0, &[]));
        assert_eq!(many.notice.title, "Moved 3 items");
        assert_eq!(many.select, ["a", "b", "c"]);
    }

    #[test]
    fn a_lone_renamed_item_is_named_as_it_now_is() {
        let response = to_paste_response(&summary(dto::Paste::Copy, &["a (1).jpg"], 1, &[]));

        assert_eq!(response.notice.title, "Copied as “a (1).jpg”");
        assert!(
            response.notice.details.is_empty(),
            "the title already says it"
        );
    }

    #[test]
    fn several_items_count_the_renamed_ones() {
        let one = to_paste_response(&summary(dto::Paste::Copy, &["a (1)", "b"], 1, &[]));
        assert_eq!(
            one.notice.details,
            ["1 item was renamed because its name was taken."]
        );

        let two = to_paste_response(&summary(dto::Paste::Move, &["a (1)", "b (1)", "c"], 2, &[]));
        assert_eq!(
            two.notice.details,
            ["2 items were renamed because their names were taken."]
        );
    }

    #[test]
    fn nothing_done_is_said_plainly() {
        assert_eq!(
            to_paste_response(&summary(dto::Paste::Move, &[], 0, &[]))
                .notice
                .title,
            "Already in this folder"
        );
        assert_eq!(
            to_paste_response(&summary(dto::Paste::Copy, &[], 0, &[]))
                .notice
                .title,
            "Nothing to copy"
        );
    }

    #[test]
    fn failures_are_named_with_their_reason_and_make_it_a_problem() {
        let some = to_paste_response(&summary(dto::Paste::Copy, &["a", "b"], 0, &["big.mov"]));
        assert_eq!(some.notice.title, "Copied 2 of 3 items");
        assert_eq!(
            some.notice.details,
            [format!(
                "“big.mov”: {}",
                error::to_error_response(dto::Error::ReadOnly).title
            )]
        );
        assert!(some.notice.problem);

        let none = to_paste_response(&summary(dto::Paste::Move, &[], 0, &["x"]));
        assert_eq!(none.notice.title, "Couldn’t move “x”");
        assert!(none.select.is_empty());

        let several = to_paste_response(&summary(dto::Paste::Copy, &[], 0, &["x", "y"]));
        assert_eq!(several.notice.title, "Couldn’t copy 2 items");
    }

    #[test]
    fn a_long_list_of_failures_is_cut_short_and_counted() {
        let labels = ["a", "b", "c", "d", "e"];
        let response = to_paste_response(&summary(dto::Paste::Copy, &["ok"], 0, &labels));

        assert_eq!(response.notice.details.len(), FAILURES_LISTED + 1);
        assert_eq!(response.notice.details.last().unwrap(), "and 2 more");
    }

    #[test]
    fn a_refusal_becomes_a_problem_banner_with_the_hint() {
        let refusal = error::to_error_response(dto::Error::IntoItself("Trip".into()));
        let response = to_paste_failure(&refusal);

        assert_eq!(response.notice.title, refusal.title);
        assert_eq!(response.notice.details, std::slice::from_ref(&refusal.hint));
        assert!(response.notice.problem);
        assert!(response.select.is_empty());
    }
}
