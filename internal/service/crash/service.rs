use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::PanicHookInfo;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dto::CrashNote;

/// Reports kept, newest first; older ones are deleted whenever one is written
/// or the user has been told.
pub const MOST_KEPT: usize = 20;
/// Reports one run writes: a panic in a loop would otherwise fill the disk.
const MOST_WRITTEN: u64 = 5;
/// The panic's own message, cut here (it can be as long as a `format!` made it).
const MESSAGE_LIMIT: usize = 2_000;
const BACKTRACE_LIMIT: usize = 48 * 1024;
/// A report's header is the first few lines; no more than this is read to find it.
const HEADER_READ: u64 = 4096;
const LOCATION_LIMIT: usize = 200;
/// The line of a report that holds the place in the code (0-based), after the
/// title, version, time and thread.
const LOCATION_LINE: usize = 4;

/// Panics seen in this process, written or not.
static PANICS: AtomicU64 = AtomicU64::new(0);
/// Reports written in this process (`MOST_WRITTEN`).
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static LAST: Mutex<Option<CrashNote>> = Mutex::new(None);

thread_local! {
    /// Set while the hook runs on this thread: a panic inside it must not
    /// start the hook again.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// How many panics this process has had. A panic on the main thread ends the
/// process; one in a task or a worker thread leaves it running with that work
/// gone, which this tells the app.
pub fn panics_so_far() -> u64 {
    PANICS.load(Ordering::SeqCst)
}

/// The latest panic in this process.
fn last_panic() -> Option<CrashNote> {
    LAST.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Crash reports: a panic leaves a text file in a folder of its own, and the
/// next run can say so. Nothing leaves the Mac.
pub struct Crashes {
    dir: PathBuf,
}

impl Crashes {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Makes every panic in the process write a report, then go on as it
    /// would have (the previous hook prints it). Called once, first thing.
    pub fn install(&self, version: &'static str) {
        prune(&self.dir);
        let dir = self.dir.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            on_panic(&dir, version, info);
            previous(info);
        }));
    }

    /// The reports kept, newest first, reported or not.
    pub fn notes(&self) -> Vec<CrashNote> {
        reports(&self.dir)
            .into_iter()
            .take(MOST_KEPT)
            .map(|report| report.note())
            .collect()
    }

    /// Reports of an earlier run (or of this one) the user was not told about.
    pub fn unreported(&self) -> Vec<CrashNote> {
        self.notes()
            .into_iter()
            .filter(|note| !note.reported)
            .collect()
    }

    /// The user has been told: each report is renamed `reported-…`, so the
    /// next start does not say it again and the file is still there to read.
    pub fn mark_reported(&self) {
        for report in reports(&self.dir) {
            if !report.reported {
                let _ = std::fs::rename(&report.path, report.path_when_reported());
            }
        }
        prune(&self.dir);
    }

    /// The latest panic of this process, with where its report went.
    pub fn last_panic(&self) -> Option<CrashNote> {
        last_panic()
    }
}

/// A file in the reports' folder, by its name alone.
struct Report {
    reported: bool,
    at_ms: u64,
    /// Told apart from another report written in the same millisecond.
    number: u32,
    path: PathBuf,
}

impl Report {
    fn note(&self) -> CrashNote {
        CrashNote {
            at_ms: self.at_ms,
            location: read_location(&self.path),
            reported: self.reported,
            file: Some(self.path.clone()),
        }
    }

    fn path_when_reported(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .replacen("crash-", "reported-", 1);
        self.path.with_file_name(name)
    }
}

/// "crash-<ms>.txt", "crash-<ms>-<n>.txt" and the same as "reported-…": the
/// only files this module reads, renames or deletes.
fn parse_name(name: &str) -> Option<(bool, u64, u32)> {
    let (reported, rest) = if let Some(rest) = name.strip_prefix("crash-") {
        (false, rest)
    } else {
        (true, name.strip_prefix("reported-")?)
    };
    let rest = rest.strip_suffix(".txt")?;
    let (ms, number) = match rest.split_once('-') {
        Some((ms, number)) => (ms, number),
        None => (rest, "0"),
    };
    let all_digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(ms) || !all_digits(number) {
        return None;
    }
    Some((reported, ms.parse().ok()?, number.parse().ok()?))
}

/// Every report in `dir`, newest first. A folder that isn't there, or a file
/// that isn't a regular file of the right name, is no report.
fn reports(dir: &Path) -> Vec<Report> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Report> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name();
            let (reported, at_ms, number) = parse_name(name.to_str()?)?;
            Some(Report {
                reported,
                at_ms,
                number,
                path: entry.path(),
            })
        })
        .collect();
    found.sort_by_key(|report| std::cmp::Reverse((report.at_ms, report.number)));
    found
}

/// Deletes all but the `MOST_KEPT` newest reports.
fn prune(dir: &Path) {
    for report in reports(dir).into_iter().skip(MOST_KEPT) {
        let _ = std::fs::remove_file(&report.path);
    }
}

/// The place in the code a report says, read from its header only and made
/// safe to show: the file may have been edited by hand.
fn read_location(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut head = Vec::new();
    if file.take(HEADER_READ).read_to_end(&mut head).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&head)
        .lines()
        .nth(LOCATION_LINE)
        .and_then(|line| line.strip_prefix("Location: "))
        .map_or_else(String::new, |location| one_line(location, LOCATION_LIMIT))
}

/// `text` as one short line: anything that could end a line, move the text
/// around or act as a control (a direction override, a line separator)
/// becomes `?`, and what is past `limit` characters is cut.
fn one_line(text: &str, limit: usize) -> String {
    text.chars()
        .take(limit)
        .map(|c| {
            let odd = c.is_control()
                || matches!(c, '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}');
            if odd { '?' } else { c }
        })
        .collect()
}

/// The code position of a panic, without anything of this Mac: what a
/// release built here has in its panics is a path under the builder's home.
/// A file of this workspace is shown from its root, a dependency from its own
/// folder in the registry, the standard library from `library/`; anything
/// else by its file name alone.
fn short_file(file: &str) -> String {
    short_file_in(file, Path::new(env!("CARGO_MANIFEST_DIR")).parent())
}

fn short_file_in(file: &str, workspace: Option<&Path>) -> String {
    let path = Path::new(file);
    if let Some(rest) = workspace.and_then(|root| path.strip_prefix(root).ok()) {
        return rest.to_string_lossy().into_owned();
    }
    // The workspace's own files come as paths from its root, unless the build
    // began somewhere else (`../`).
    if path.is_relative()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return file.to_owned();
    }
    if let Some(at) = file.find("/registry/src/") {
        // "<index folder>/<crate>-<version>/src/…": the index folder's name
        // says nothing.
        let after = &file[at + "/registry/src/".len()..];
        if let Some((_, crate_file)) = after.split_once('/') {
            return crate_file.to_owned();
        }
    }
    if file.starts_with("/rustc/")
        && let Some(at) = file.find("/library/")
    {
        return file[at + 1..].to_owned();
    }
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// `text` cut to at most `limit` bytes, at a character.
fn cut(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0))
}

/// What a panic carried: a `&str` or a `String` for `panic!`, anything else
/// has no message to show.
fn message_of(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_owned())
}

/// The hook's whole work, which must never panic (that would abort the
/// process), so a panic inside it is caught, and never starts it again.
fn on_panic(dir: &Path, version: &str, info: &PanicHookInfo<'_>) {
    inside_the_hook(|| record(dir, version, info));
}

/// Runs `work` as the hook's: not at all when the hook is already running on
/// this thread, and a panic in it goes no further.
fn inside_the_hook(work: impl FnOnce()) {
    if IN_HOOK.with(|inside| inside.replace(true)) {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
    IN_HOOK.with(|inside| inside.set(false));
}

fn record(dir: &Path, version: &str, info: &PanicHookInfo<'_>) {
    let at_ms = now_ms();
    let location = info.location().map_or_else(String::new, |place| {
        format!(
            "{}:{}:{}",
            short_file(place.file()),
            place.line(),
            place.column()
        )
    });
    let location = one_line(&location, LOCATION_LIMIT);
    let thread = std::thread::current();
    let thread = one_line(thread.name().unwrap_or("(unnamed)"), 64);

    PANICS.fetch_add(1, Ordering::SeqCst);
    let file = if WRITTEN.fetch_add(1, Ordering::SeqCst) < MOST_WRITTEN {
        let message = message_of(info);
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        write_report(
            dir,
            &Written {
                version,
                at_ms,
                thread: &thread,
                location: &location,
                message: cut(&message, MESSAGE_LIMIT),
                backtrace: cut(&backtrace, BACKTRACE_LIMIT),
            },
        )
        .ok()
    } else {
        None
    };
    if file.is_some() {
        prune(dir);
    }
    *LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CrashNote {
        at_ms,
        location,
        reported: false,
        file,
    });
}

/// What a report says.
struct Written<'a> {
    version: &'a str,
    at_ms: u64,
    thread: &'a str,
    location: &'a str,
    message: &'a str,
    backtrace: &'a str,
}

/// The report's text. Its first lines are fixed (`LOCATION_LINE`), and the
/// thread and location are single lines already, so the message can't write
/// the header.
fn report_text(report: &Written<'_>) -> String {
    let when = chrono::DateTime::from_timestamp_millis(i64::try_from(report.at_ms).unwrap_or(0))
        .map_or_else(String::new, |at| {
            at.format("%Y-%m-%d %H:%M:%S UTC").to_string()
        });
    format!(
        "open-mtp-client crash report\n\
         Version: {version}\n\
         When: {ms} ({when})\n\
         Thread: {thread}\n\
         Location: {location}\n\
         \n\
         This file stays on your Mac. The message below can hold names from your\n\
         files: read it before you send it to anyone.\n\
         \n\
         Message:\n{message}\n\
         \n\
         Backtrace:\n{backtrace}\n",
        version = report.version,
        ms = report.at_ms,
        thread = report.thread,
        location = report.location,
        message = report.message,
        backtrace = report.backtrace,
    )
}

/// Writes a new report in `dir`, never over another, readable by this user only.
fn write_report(dir: &Path, report: &Written<'_>) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let text = report_text(report);
    for number in 0..100u32 {
        let name = if number == 0 {
            format!("crash-{}.txt", report.at_ms)
        } else {
            format!("crash-{}-{number}.txt", report.at_ms)
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(text.as_bytes()) {
                    let _ = std::fs::remove_file(&path);
                    return Err(err);
                }
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn written(at_ms: u64, location: &str) -> Written<'_> {
        Written {
            version: "0.1.0",
            at_ms,
            thread: "tokio-runtime-worker",
            location,
            message: "index out of bounds: the len is 3 but the index is 7",
            backtrace: "   0: some::frame\n   1: another::frame",
        }
    }

    fn put(dir: &Path, name: &str, location: &str) {
        fs::write(
            dir.join(name),
            format!("open-mtp-client crash report\nVersion: 0.1.0\nWhen: 1\nThread: t\nLocation: {location}\n\nMessage:\nx\n"),
        )
        .unwrap();
    }

    #[test]
    fn a_report_is_named_by_its_time_and_never_written_over_another() {
        let dir = tempfile::tempdir().unwrap();

        let first = write_report(dir.path(), &written(1_000, "a.rs:1:1")).unwrap();
        let second = write_report(dir.path(), &written(1_000, "b.rs:2:2")).unwrap();

        assert_eq!(first.file_name().unwrap(), "crash-1000.txt");
        assert_eq!(second.file_name().unwrap(), "crash-1000-1.txt");
        assert!(fs::read_to_string(&first).unwrap().contains("a.rs:1:1"));
        assert!(fs::read_to_string(&second).unwrap().contains("b.rs:2:2"));
    }

    #[test]
    fn a_report_is_for_this_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();

        let path = write_report(dir.path(), &written(5, "a.rs:1:1")).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_report_says_what_happened_where_and_when() {
        let text = report_text(&written(1_792_000_000_000, "internal/x.rs:9:3"));

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "open-mtp-client crash report");
        assert_eq!(lines[1], "Version: 0.1.0");
        assert!(
            lines[2].starts_with("When: 1792000000000 (2026-10-"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "Thread: tokio-runtime-worker");
        assert_eq!(lines[LOCATION_LINE], "Location: internal/x.rs:9:3");
        assert!(text.contains("Message:\nindex out of bounds"));
        assert!(text.contains("Backtrace:\n   0: some::frame"));
        assert!(text.contains("read it before you send it"));
    }

    #[test]
    fn the_notes_come_newest_first_with_where_each_happened() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "crash-100.txt", "old.rs:1:1");
        put(dir.path(), "reported-300.txt", "newest.rs:3:3");
        put(dir.path(), "crash-200.txt", "mid.rs:2:2");
        put(dir.path(), "crash-200-1.txt", "later-in-the-same-ms.rs:4:4");
        let crashes = Crashes::new(dir.path().to_path_buf());

        let notes = crashes.notes();

        let seen: Vec<(u64, &str, bool)> = notes
            .iter()
            .map(|note| (note.at_ms, note.location.as_str(), note.reported))
            .collect();
        assert_eq!(
            seen,
            [
                (300, "newest.rs:3:3", true),
                (200, "later-in-the-same-ms.rs:4:4", false),
                (200, "mid.rs:2:2", false),
                (100, "old.rs:1:1", false),
            ]
        );
        assert_eq!(notes[0].file, Some(dir.path().join("reported-300.txt")));
        assert_eq!(crashes.unreported().len(), 3);
    }

    #[test]
    fn what_is_not_a_report_is_not_read_renamed_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "crash-.txt",
            "crash-12a.txt",
            "crash-12.txt.bak",
            "crash-1-x.txt",
            "crash-1-2-3.txt",
            "notes.txt",
            "reported-.txt",
            "xcrash-5.txt",
            // Numbers `parse` takes that are not digits.
            "crash-+5.txt",
            "crash-5-+2.txt",
            "reported-+7.txt",
            ".DS_Store",
        ] {
            fs::write(dir.path().join(name), "keep me").unwrap();
        }
        fs::create_dir(dir.path().join("crash-77.txt")).unwrap();
        let crashes = Crashes::new(dir.path().to_path_buf());

        assert!(crashes.notes().is_empty(), "{:?}", crashes.notes());
        crashes.mark_reported();

        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 13);
        assert!(dir.path().join("crash-77.txt").is_dir());
    }

    #[test]
    fn a_folder_that_is_not_there_has_no_reports() {
        let crashes = Crashes::new(PathBuf::from("/nonexistent/open-mtp-client/crashes"));

        assert!(crashes.notes().is_empty());
        assert!(crashes.unreported().is_empty());
        crashes.mark_reported();
    }

    #[test]
    fn telling_the_user_renames_the_reports_and_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "crash-100.txt", "a.rs:1:1");
        put(dir.path(), "crash-200-1.txt", "b.rs:2:2");
        put(dir.path(), "reported-50.txt", "c.rs:3:3");
        let crashes = Crashes::new(dir.path().to_path_buf());

        crashes.mark_reported();

        assert!(crashes.unreported().is_empty());
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["reported-100.txt", "reported-200-1.txt", "reported-50.txt"]
        );
        assert!(
            fs::read_to_string(dir.path().join("reported-100.txt"))
                .unwrap()
                .contains("a.rs:1:1")
        );
    }

    #[test]
    fn only_the_newest_reports_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for ms in 1..=(MOST_KEPT as u64 + 5) {
            put(dir.path(), &format!("crash-{ms}.txt"), "a.rs:1:1");
        }
        put(dir.path(), "notes.txt", "");

        prune(dir.path());

        let kept = reports(dir.path());
        assert_eq!(kept.len(), MOST_KEPT);
        assert_eq!(kept[0].at_ms, MOST_KEPT as u64 + 5);
        assert_eq!(kept[MOST_KEPT - 1].at_ms, 6, "the five oldest went");
        assert!(dir.path().join("notes.txt").exists(), "and nothing else");
    }

    #[test]
    fn a_location_in_a_hand_edited_report_is_made_safe_to_show() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            "crash-1.txt",
            "a.rs:1:1\u{202E}evil\u{2028}more",
        );
        put(dir.path(), "crash-2.txt", &"x".repeat(500));
        // No Location line at all, and not text.
        fs::write(dir.path().join("crash-3.txt"), [0xFF, 0xFE, 0x00, 0x41]).unwrap();
        fs::write(dir.path().join("crash-4.txt"), "").unwrap();
        let notes = Crashes::new(dir.path().to_path_buf()).notes();

        assert_eq!(notes[3].location, "a.rs:1:1?evil?more");
        assert_eq!(notes[2].location, "x".repeat(LOCATION_LIMIT));
        assert_eq!(notes[1].location, "");
        assert_eq!(notes[0].location, "");
    }

    #[test]
    fn a_message_cannot_write_the_header_of_its_report() {
        let dir = tempfile::tempdir().unwrap();
        let message = "boom\nLocation: /Users/someone/secret.rs:1:1\nWhen: 1";

        write_report(
            dir.path(),
            &Written {
                message,
                ..written(10, "internal/x.rs:1:1")
            },
        )
        .unwrap();

        let notes = Crashes::new(dir.path().to_path_buf()).notes();
        assert_eq!(notes[0].location, "internal/x.rs:1:1");
    }

    #[test]
    fn a_position_in_the_code_never_shows_a_path_on_the_builders_mac() {
        let root = Path::new("/Users/someone/work/mtprust");
        let at = |file: &str| short_file_in(file, Some(root));

        // This workspace, as an absolute path or as the relative one rustc gives.
        assert_eq!(
            at("/Users/someone/work/mtprust/internal/service/x.rs"),
            "internal/service/x.rs"
        );
        assert_eq!(
            at("cmd/open-mtp-client/app.rs"),
            "cmd/open-mtp-client/app.rs"
        );
        assert_eq!(
            at("vendor/mtp-rs/src/ptp/container.rs"),
            "vendor/mtp-rs/src/ptp/container.rs"
        );
        // A dependency, from its own folder.
        assert_eq!(
            at(
                "/Users/someone/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.40.0/src/runtime/mod.rs"
            ),
            "tokio-1.40.0/src/runtime/mod.rs"
        );
        // The standard library.
        assert_eq!(
            at("/rustc/3a5b8c1d/library/core/src/option.rs"),
            "library/core/src/option.rs"
        );
        // Anything else by its file name alone, a relative path out of the
        // workspace included.
        assert_eq!(at("/Users/someone/other/project/lib.rs"), "lib.rs");
        assert_eq!(at("../../Users/someone/other/lib.rs"), "lib.rs");
        assert_eq!(at(""), "");
        // And what this build says of its own files holds no home folder.
        assert!(
            !short_file(file!()).starts_with('/'),
            "{}",
            short_file(file!())
        );
    }

    #[test]
    fn the_hook_does_not_start_itself_again_and_keeps_a_panic_in_it_to_itself() {
        use std::cell::Cell;
        let outer = Cell::new(false);
        let inner = Cell::new(false);

        inside_the_hook(|| {
            outer.set(true);
            // A panic while the hook runs would start it again.
            inside_the_hook(|| inner.set(true));
        });

        assert!(outer.get(), "the work ran");
        assert!(!inner.get(), "but not again inside itself");

        // A panic in the work stays in it, and the next use is not shut out.
        inside_the_hook(|| panic!("broke inside the hook"));
        let after = Cell::new(false);
        inside_the_hook(|| after.set(true));
        assert!(after.get());
    }

    #[test]
    fn long_text_is_cut_at_a_character() {
        assert_eq!(cut("abc", 10), "abc");
        assert_eq!(cut("abcdef", 3), "abc");
        // "é" is two bytes: a cut in the middle of it goes back to before it.
        assert_eq!(cut("aé", 2), "a");
        assert_eq!(cut("aé", 3), "aé");
        assert_eq!(cut("", 0), "");
    }
}
