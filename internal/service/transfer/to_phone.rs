//! Moving Mac items to the phone: send them, read every file back from the
//! phone, and only then put the originals in the Trash, as Finder's Move to
//! Trash would.
//!
//! The Trash is where a mistake would cost the user something, so the rules
//! are strict. An item goes only if
//! - every file in it is on the phone and the phone reports the size that was
//!   sent (or has it already, same size and date): a phone that gives no size,
//!   as some do for 4 GB or more, leaves the original alone;
//! - it is still exactly what was read before sending: a file that grew or was
//!   touched meanwhile, or a folder that gained something, stays, because the
//!   copy is not what is on the Mac now;
//! - nothing in it was left out of the copy: uploads skip links to folders and
//!   things that are neither files nor folders, and the folder holding them
//!   would take them to the Trash unseen. (`.DS_Store` and `._*` files are
//!   Mac clutter that uploads leave out on purpose; they go with their folder.)
//!
//! One Trash call is made per selected item, not per file, because the Trash
//! is slow (it asks Finder). A failed upload, a cancelled one or a lost phone
//! trashes nothing.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use super::Service;
use super::upload::{Delivered, LocalNode, Options, scan};
use crate::dto::{
    Cancel, Conflict, Direction, Error, FileOutcome, FileReport, Kept, Original, Paste, Removal,
    TransferProgress, TransferSummary, UploadRequest, Why,
};
use crate::service::local;

/// Moves one Mac item to the Trash; slow, so it runs off the async threads.
pub(super) type Trash = Arc<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

impl Service {
    /// This service, able to put Mac items in the Trash, which a move to the
    /// phone needs. Without it a move is refused (`Error::Unsupported`) before
    /// anything is sent, so nothing is ever copied that could not be finished.
    pub fn with_trash(
        mut self,
        trash: impl Fn(&Path) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.trash = Some(Arc::new(trash));
        self
    }

    /// `upload`, or for `Paste::Move` an upload that then moves each original
    /// to the Trash once its copy is checked (see the module docs). A name
    /// taken on the phone is handled as `conflict` says; an item left alone
    /// because of it stays on the Mac and is not an error.
    pub async fn upload_as(
        &self,
        mode: Paste,
        request: &UploadRequest,
        conflict: Conflict,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        if mode == Paste::Copy {
            return self.upload(request, conflict, cancel, progress).await;
        }
        // A move puts what it sent in the Trash: an original whose copy was
        // kept back, being older, must not go.
        if conflict == Conflict::ReplaceOlder {
            return Err(Error::Unsupported);
        }
        let Some(trash) = self.trash.clone() else {
            return Err(Error::Unsupported);
        };
        let moving = |mut report: TransferProgress| {
            report.moving = true;
            progress(report);
        };
        let options = Options {
            // A retry finds the files an earlier attempt already sent.
            skip_identical: true,
            verify: true,
        };
        let sent = self
            .upload_checked(request, conflict, options, cancel, &moving)
            .await?;
        let mut summary = sent.summary;
        let removal =
            trash_originals(&trash, &request.sources, &sent.delivered, cancel, &moving).await;
        note_originals(&mut summary.report, &sent.report_tops, &removal);
        summary.removal = Some(removal);
        Ok(summary)
    }
}

/// Says in the report what became of each file's original, by the selected
/// item it is in (`tops`, one per line): kept with its reason, left where it
/// was (not sent whole, skipped, or not reached), or in the Trash.
fn note_originals(report: &mut [FileReport], tops: &[String], removal: &Removal) {
    for (line, top) in report.iter_mut().zip(tops) {
        let kept = removal.kept.iter().find(|kept| kept.label == *top);
        let left = removal.left.iter().any(|source| file_name(source) == *top);
        line.original = Some(match kept {
            Some(kept) => Original::Kept(kept.why.clone()),
            None if left || line.outcome == FileOutcome::Skipped => Original::Stayed,
            None => Original::Removed,
        });
    }
}

/// Puts the originals of what was sent in the Trash, the ones that may go.
async fn trash_originals(
    trash: &Trash,
    sources: &[String],
    delivered: &[Delivered],
    cancel: &Cancel,
    progress: &(impl Fn(TransferProgress) + Send + Sync),
) -> Removal {
    let mut removal = Removal {
        from_mac: true,
        ..Removal::default()
    };
    // Looked up by path once, not searched for by each of what may be
    // thousands of selected items.
    let by_path: HashMap<&Path, &Delivered> = delivered
        .iter()
        .map(|sent| (sent.node.path.as_path(), sent))
        .collect();
    let mut candidates = Vec::new();
    for source in sources {
        match by_path.get(Path::new(source)).copied() {
            // Left out of the upload: a link to a folder, a socket.
            None => removal.kept.push(Kept {
                label: file_name(source),
                why: Why::NotCopied,
            }),
            // The user chose to leave it be; it is neither moved nor owed.
            Some(sent) if sent.skipped => {}
            // A file in it failed: the failures list says which.
            Some(sent) if sent.failed => removal.left.push(source.clone()),
            Some(sent) if !sent.confirmed => {
                removal.kept.push(Kept {
                    label: sent.name.clone(),
                    why: Why::Unchecked,
                });
                removal.left.push(source.clone());
            }
            Some(sent) => candidates.push((source, &sent.node)),
        }
    }

    let total = candidates.len();
    let report = |done: usize| TransferProgress {
        direction: Direction::Upload,
        removing: true,
        trashing: true,
        files_done: done,
        files_total: total,
        ..TransferProgress::default()
    };
    if total > 0 {
        progress(report(0));
    }
    let mut done = 0;
    for (index, (source, node)) in candidates.iter().enumerate() {
        if cancel.is_cancelled() {
            removal.stopped = Some(Error::Cancelled);
            removal.left.extend(
                candidates[index..]
                    .iter()
                    .map(|(source, _)| (*source).clone()),
            );
            break;
        }
        match trash_one(trash, node).await {
            Ok(()) => {
                removal.removed += node.files_and_bytes().0;
                done += 1;
                progress(report(done));
            }
            Err(why) => {
                removal.kept.push(Kept {
                    label: node.name.clone(),
                    why,
                });
                removal.left.push((*source).clone());
            }
        }
    }
    removal
}

/// Checks that `node` is still what was copied, then moves it to the Trash.
async fn trash_one(trash: &Trash, node: &Arc<LocalNode>) -> Result<(), Why> {
    may_go(node).await?;
    let (trash, path) = (Arc::clone(trash), node.path.clone());
    let trashed = tokio::task::spawn_blocking(move || trash(&path)).await;
    match trashed {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(Why::Refused(local::mapper::to_error(
            &err,
            &node.path.to_string_lossy(),
        ))),
        Err(err) => Err(Why::Refused(Error::General(err.to_string()))),
    }
}

/// Whether the Mac item is still just what was copied (see the module docs):
/// read again now and compared with what was read before sending.
async fn may_go(node: &LocalNode) -> Result<(), Why> {
    if left_out(node) {
        return Err(Why::NotEmpty);
    }
    let mut count = 0;
    let now = match scan(node.path.clone(), node.name.clone(), 0, &mut count).await {
        Ok(Some(now)) => now,
        // Gone, unreadable, or no longer a file or a folder.
        _ => return Err(Why::Changed),
    };
    match difference(node, &now) {
        Difference::Same => Ok(()),
        Difference::Changed => Err(Why::Changed),
        Difference::New => Err(Why::NotEmpty),
    }
}

/// Something under `node` was not copied because it can't be.
fn left_out(node: &LocalNode) -> bool {
    node.left_out > 0 || node.children.iter().any(left_out)
}

/// How a Mac item differs from what was read before sending, the worse one
/// winning: something new that was never copied outweighs a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Difference {
    Same,
    /// Something that was copied is different or gone.
    Changed,
    /// Something is there that was not copied.
    New,
}

fn difference(before: &LocalNode, now: &LocalNode) -> Difference {
    if before.is_folder != now.is_folder {
        return Difference::Changed;
    }
    if !before.is_folder {
        return if before.size == now.size && before.modified == now.modified {
            Difference::Same
        } else {
            Difference::Changed
        };
    }
    let mut worst = if now.left_out > before.left_out {
        Difference::New
    } else {
        Difference::Same
    };
    let was: HashMap<&str, &LocalNode> = before
        .children
        .iter()
        .map(|child| (child.name.as_str(), child))
        .collect();
    for child in &now.children {
        let seen = match was.get(child.name.as_str()) {
            Some(earlier) => difference(earlier, child),
            None => Difference::New,
        };
        worst = worst.max(seen);
    }
    let still_there: HashSet<&str> = now.children.iter().map(|c| c.name.as_str()).collect();
    if before
        .children
        .iter()
        .any(|child| !still_there.contains(child.name.as_str()))
    {
        worst = worst.max(Difference::Changed);
    }
    worst
}

fn file_name(source: &str) -> String {
    Path::new(source).file_name().map_or_else(
        || source.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn each_lines_original_follows_its_selected_item() {
        let line = |path: &str, outcome| FileReport {
            path: path.into(),
            from: None,
            outcome,
            original: None,
        };
        let mut report = vec![
            line("Trip/a.jpg", FileOutcome::Copied { checked: true }),
            line("Trip/b.jpg", FileOutcome::Copied { checked: true }),
            line("notes.txt", FileOutcome::Copied { checked: false }),
            line("song.mp3", FileOutcome::Failed(Error::MacDiskFull)),
            line("old.txt", FileOutcome::Skipped),
        ];
        let tops: Vec<String> = ["Trip", "Trip", "notes.txt", "song.mp3", "old.txt"]
            .map(String::from)
            .to_vec();
        let removal = Removal {
            kept: vec![Kept {
                label: "notes.txt".into(),
                why: Why::Unchecked,
            }],
            left: vec!["/Users/me/notes.txt".into(), "/Users/me/song.mp3".into()],
            ..Removal::default()
        };

        note_originals(&mut report, &tops, &removal);

        let originals: Vec<Option<Original>> =
            report.into_iter().map(|line| line.original).collect();
        assert_eq!(
            originals,
            [
                Some(Original::Removed),
                Some(Original::Removed),
                Some(Original::Kept(Why::Unchecked)),
                Some(Original::Stayed),
                Some(Original::Stayed),
            ]
        );
    }
    use crate::service::device;
    use crate::service::explorer;
    use crate::service::testing::virtual_phone;

    /// A phone with notes.txt ("old") and an empty Music folder, a Mac folder
    /// to move from, and a Trash that is a folder of the test's own, so the
    /// user's Trash is never touched.
    struct Up {
        phone: tempfile::TempDir,
        mac: tempfile::TempDir,
        bin: tempfile::TempDir,
        service: Service,
        explorer: Arc<explorer::Service>,
        trashed: Arc<Mutex<Vec<PathBuf>>>,
        storage: u64,
        serial: String,
    }

    async fn up() -> Up {
        up_with(true).await
    }

    async fn up_with(trash: bool) -> Up {
        let phone = tempfile::tempdir().unwrap();
        fs::create_dir_all(phone.path().join("Music")).unwrap();
        fs::write(phone.path().join("notes.txt"), b"old").unwrap();
        let device = Arc::new(device::Service::new(device::Source::Virtual(
            virtual_phone(phone.path()),
        )));
        let serial = device.connect().await.unwrap().serial_number;
        let storage = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let bin = tempfile::tempdir().unwrap();
        let trashed = Arc::new(Mutex::new(Vec::new()));
        let mut service = Service::new(device, Arc::clone(&explorer));
        if trash {
            service = service.with_trash(recorder(bin.path(), &trashed, |_| false));
        }
        Up {
            phone,
            mac: tempfile::tempdir().unwrap(),
            bin,
            service,
            explorer,
            trashed,
            storage,
            serial,
        }
    }

    /// A Trash that moves the item into `bin` and notes it, refusing the items
    /// `refuse` says to.
    fn recorder(
        bin: &Path,
        log: &Arc<Mutex<Vec<PathBuf>>>,
        refuse: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> impl Fn(&Path) -> std::io::Result<()> + Send + Sync + 'static {
        let (bin, log) = (bin.to_path_buf(), Arc::clone(log));
        let count = AtomicUsize::new(0);
        move |path| {
            if refuse(path) {
                return Err(std::io::Error::other("Trash is full"));
            }
            log.lock().unwrap().push(path.to_path_buf());
            let n = count.fetch_add(1, Ordering::Relaxed);
            fs::rename(
                path,
                bin.join(format!("{n}-{}", file_name(&path.to_string_lossy()))),
            )
        }
    }

    impl Up {
        fn write(&self, path: &str, content: &[u8]) {
            let path = self.mac.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn mac_path(&self, name: &str) -> String {
            self.mac.path().join(name).to_string_lossy().into_owned()
        }

        fn request(&self, names: &[&str], to: &str) -> UploadRequest {
            UploadRequest {
                sources: names.iter().map(|name| self.mac_path(name)).collect(),
                storage_id: self.storage,
                destination: to.into(),
            }
        }

        async fn move_with(
            &self,
            names: &[&str],
            to: &str,
            conflict: Conflict,
            cancel: &Cancel,
            progress: impl Fn(TransferProgress) + Send + Sync,
        ) -> Result<TransferSummary, Error> {
            self.service
                .upload_as(
                    Paste::Move,
                    &self.request(names, to),
                    conflict,
                    cancel,
                    progress,
                )
                .await
        }

        async fn mv(&self, names: &[&str], to: &str) -> Result<TransferSummary, Error> {
            self.move_with(names, to, Conflict::KeepBoth, &Cancel::new(), |_| {})
                .await
        }

        fn on_phone(&self, path: &str) -> bool {
            self.phone.path().join(path).exists()
        }

        fn on_mac(&self, path: &str) -> bool {
            self.mac.path().join(path).exists()
        }

        fn trash_calls(&self) -> usize {
            self.trashed.lock().unwrap().len()
        }
    }

    #[tokio::test]
    async fn a_move_sends_each_item_and_then_puts_its_original_in_the_trash() {
        let u = up().await;
        u.write("a.txt", b"aaaa");
        u.write("Trip/1.jpg", b"11");
        u.write("Trip/day2/2.jpg", b"222");

        let summary = u.mv(&["a.txt", "Trip"], "/Music").await.unwrap();

        assert_eq!(
            fs::read(u.phone.path().join("Music/a.txt")).unwrap(),
            b"aaaa"
        );
        assert_eq!(
            fs::read(u.phone.path().join("Music/Trip/day2/2.jpg")).unwrap(),
            b"222"
        );
        assert!(!u.on_mac("a.txt") && !u.on_mac("Trip"), "both went");
        assert_eq!(
            u.trash_calls(),
            2,
            "one call per selected item, not per file"
        );
        let removal = summary.removal.unwrap();
        assert!(removal.from_mac);
        assert_eq!(removal.removed, 3, "files in what went to the Trash");
        assert!(removal.kept.is_empty() && removal.left.is_empty());
        assert_eq!(removal.stopped, None);
        assert_eq!((summary.files, summary.bytes), (3, 9));

        // Each file read back from the phone, and its original in the Trash.
        let mut report: Vec<(&str, &FileOutcome, Option<&Original>)> = summary
            .report
            .iter()
            .map(|line| (line.path.as_str(), &line.outcome, line.original.as_ref()))
            .collect();
        report.sort_by_key(|line| line.0);
        let checked = FileOutcome::Copied { checked: true };
        assert_eq!(
            report,
            [
                ("Trip/1.jpg", &checked, Some(&Original::Removed)),
                ("Trip/day2/2.jpg", &checked, Some(&Original::Removed)),
                ("a.txt", &checked, Some(&Original::Removed)),
            ]
        );
    }

    #[tokio::test]
    async fn a_copy_puts_nothing_in_the_trash() {
        let u = up().await;
        u.write("a.txt", b"aaaa");

        let summary = u
            .service
            .upload_as(
                Paste::Copy,
                &u.request(&["a.txt"], "/Music"),
                Conflict::KeepBoth,
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert!(u.on_phone("Music/a.txt") && u.on_mac("a.txt"));
        assert_eq!(summary.removal, None);
        assert_eq!(u.trash_calls(), 0);
    }

    #[tokio::test]
    async fn a_move_with_no_trash_is_refused_before_anything_is_sent() {
        let u = up_with(false).await;
        u.write("a.txt", b"aaaa");

        let result = u.mv(&["a.txt"], "/Music").await;

        assert_eq!(result.err(), Some(Error::Unsupported));
        assert!(!u.on_phone("Music/a.txt"), "nothing was copied");
        assert!(u.on_mac("a.txt"));
    }

    #[tokio::test]
    async fn a_move_that_would_replace_only_older_files_is_refused_before_anything_is_sent() {
        let u = up().await;
        u.write("a.txt", b"aaaa");

        let result = u
            .move_with(
                &["a.txt"],
                "/Music",
                Conflict::ReplaceOlder,
                &Cancel::new(),
                |_| {},
            )
            .await;

        assert_eq!(result.err(), Some(Error::Unsupported));
        assert!(!u.on_phone("Music/a.txt"));
        assert!(u.on_mac("a.txt"), "nothing went to the Trash");
        assert!(u.trashed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_trash_never_hears_of_an_item_the_phone_could_not_confirm() {
        // The phone says nothing about the object it was just given: the copy
        // can't be checked, so the original stays (a phone that gives no size
        // for 4 GB or more is the same case).
        let u = up().await;
        u.write("a.txt", b"aaaa");
        let mut highest = 0;
        for folder in ["/", "/Music"] {
            for entry in u.explorer.list(u.storage, folder, true).await.unwrap() {
                highest = highest.max(entry.id);
            }
        }
        for next in highest + 1..=highest + 8 {
            mtp_rs::force_object_info_error(&u.serial, mtp_rs::mtp::ObjectHandle(next), None);
        }

        let summary = u.mv(&["a.txt"], "/Music").await.unwrap();

        assert!(u.on_phone("Music/a.txt"), "the copy is there");
        assert!(u.on_mac("a.txt"), "and so is the original");
        let removal = summary.removal.unwrap();
        assert_eq!(removal.removed, 0);
        assert_eq!(
            removal.kept,
            [Kept {
                label: "a.txt".into(),
                why: Why::Unchecked
            }]
        );
        assert_eq!(removal.left, [u.mac_path("a.txt")]);
        assert_eq!(u.trash_calls(), 0);
    }

    #[tokio::test]
    async fn a_file_touched_after_it_was_read_stays() {
        let u = up().await;
        u.write("a.txt", b"aaaa");
        u.write("b.txt", b"bbbb");
        let touched = u.mac.path().join("a.txt");

        // The copy is done and the Trash is about to be asked: change a file.
        let summary = u
            .move_with(
                &["a.txt", "b.txt"],
                "/Music",
                Conflict::KeepBoth,
                &Cancel::new(),
                |p| {
                    if p.trashing && p.files_done == 0 {
                        fs::write(&touched, b"aaaa and more").unwrap();
                    }
                },
            )
            .await
            .unwrap();

        assert!(u.on_mac("a.txt"), "what changed is not what was copied");
        assert!(!u.on_mac("b.txt"), "the other one went");
        let removal = summary.removal.unwrap();
        assert_eq!(
            removal.kept,
            [Kept {
                label: "a.txt".into(),
                why: Why::Changed
            }]
        );
        assert_eq!(removal.left, [u.mac_path("a.txt")]);
        assert_eq!(removal.removed, 1);
    }

    #[tokio::test]
    async fn a_folder_that_gained_a_file_stays_whole() {
        let u = up().await;
        u.write("Trip/1.jpg", b"11");
        let new = u.mac.path().join("Trip/new.jpg");

        let summary = u
            .move_with(
                &["Trip"],
                "/Music",
                Conflict::KeepBoth,
                &Cancel::new(),
                |p| {
                    if p.trashing && p.files_done == 0 {
                        fs::write(&new, b"never copied").unwrap();
                    }
                },
            )
            .await
            .unwrap();

        assert!(u.on_mac("Trip/1.jpg") && u.on_mac("Trip/new.jpg"));
        assert!(u.on_phone("Music/Trip/1.jpg") && !u.on_phone("Music/Trip/new.jpg"));
        let removal = summary.removal.unwrap();
        assert_eq!(
            removal.kept,
            [Kept {
                label: "Trip".into(),
                why: Why::NotEmpty
            }]
        );
        assert_eq!(u.trash_calls(), 0);
    }

    #[tokio::test]
    async fn a_folder_with_something_uploads_leave_out_stays_whole() {
        // A link to a folder is not sent, and the folder holding it would take
        // it to the Trash unseen.
        let u = up().await;
        u.write("Trip/1.jpg", b"11");
        u.write("Elsewhere/x.jpg", b"x");
        std::os::unix::fs::symlink(
            u.mac.path().join("Elsewhere"),
            u.mac.path().join("Trip/shortcut"),
        )
        .unwrap();

        let summary = u.mv(&["Trip"], "/Music").await.unwrap();

        assert!(u.on_phone("Music/Trip/1.jpg") && !u.on_phone("Music/Trip/shortcut"));
        assert!(u.on_mac("Trip/1.jpg"), "the folder stays, link and all");
        assert!(fs::symlink_metadata(u.mac.path().join("Trip/shortcut")).is_ok());
        let removal = summary.removal.unwrap();
        assert_eq!(removal.kept.len(), 1);
        assert_eq!(removal.kept[0].why, Why::NotEmpty);
        assert_eq!(removal.left, [u.mac_path("Trip")]);
        assert_eq!(u.trash_calls(), 0);
    }

    #[tokio::test]
    async fn mac_clutter_goes_with_its_folder() {
        let u = up().await;
        u.write("Trip/1.jpg", b"11");
        u.write("Trip/.DS_Store", b"x");
        u.write("Trip/._1.jpg", b"x");

        let summary = u.mv(&["Trip"], "/Music").await.unwrap();

        assert!(u.on_phone("Music/Trip/1.jpg"));
        assert!(!u.on_phone("Music/Trip/.DS_Store"), "never sent");
        assert!(!u.on_mac("Trip"), "the folder went, clutter with it");
        assert!(summary.removal.unwrap().left.is_empty());
    }

    #[tokio::test]
    async fn an_item_that_uploads_leave_out_is_named_and_stays() {
        let u = up().await;
        u.write("Elsewhere/x.jpg", b"x");
        u.write("a.txt", b"aaaa");
        std::os::unix::fs::symlink(u.mac.path().join("Elsewhere"), u.mac.path().join("Link"))
            .unwrap();

        let summary = u.mv(&["Link", "a.txt"], "/Music").await.unwrap();

        assert!(!u.on_mac("a.txt"));
        assert!(fs::symlink_metadata(u.mac.path().join("Link")).is_ok());
        let removal = summary.removal.unwrap();
        assert_eq!(
            removal.kept,
            [Kept {
                label: "Link".into(),
                why: Why::NotCopied
            }]
        );
        assert!(
            removal.left.is_empty(),
            "a retry would leave it out again, so it is not offered"
        );
    }

    #[tokio::test]
    async fn a_name_the_user_chose_to_skip_stays_and_is_not_owed() {
        let u = up().await;
        u.write("notes.txt", b"different");
        u.write("b.txt", b"bbbb");

        let summary = u
            .move_with(
                &["notes.txt", "b.txt"],
                "/",
                Conflict::Skip,
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(fs::read(u.phone.path().join("notes.txt")).unwrap(), b"old");
        assert!(u.on_mac("notes.txt"), "skipped, so it stays");
        assert!(!u.on_mac("b.txt"));
        assert_eq!(summary.skipped, 1);
        let removal = summary.removal.unwrap();
        assert!(removal.kept.is_empty() && removal.left.is_empty());
    }

    #[tokio::test]
    async fn an_item_that_failed_to_send_stays_and_the_others_go() {
        let u = up().await;
        u.write("a.txt", b"aaaa");
        u.write("locked.txt", b"secret");
        u.write("c.txt", b"cccc");
        let locked = u.mac.path().join("locked.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let summary = u.mv(&["a.txt", "locked.txt", "c.txt"], "/Music").await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        let summary = summary.unwrap();

        assert!(!u.on_mac("a.txt") && !u.on_mac("c.txt"));
        assert!(u.on_mac("locked.txt"));
        assert_eq!(summary.failures.len(), 1);
        let removal = summary.removal.unwrap();
        assert_eq!(removal.left, [u.mac_path("locked.txt")]);
        assert!(removal.kept.is_empty(), "the failure already says why");
        assert_eq!(removal.removed, 2);
    }

    #[tokio::test]
    async fn a_move_that_ends_with_an_error_trashes_nothing() {
        // Nothing could be sent (the phone is read-only), so nothing is moved.
        let u = up().await;
        u.write("a.txt", b"aaaa");
        let cancel = Cancel::new();
        cancel.cancel();

        let result = u
            .move_with(&["a.txt"], "/Music", Conflict::KeepBoth, &cancel, |_| {})
            .await;

        assert_eq!(result.err(), Some(Error::Cancelled));
        assert!(u.on_mac("a.txt"));
        assert_eq!(u.trash_calls(), 0);
    }

    #[tokio::test]
    async fn cancelling_while_the_originals_go_stops_and_leaves_the_rest() {
        let u = up().await;
        u.write("a.txt", b"aaaa");
        u.write("b.txt", b"bbbb");
        u.write("c.txt", b"cccc");
        let cancel = Cancel::new();
        let stop = cancel.clone();

        let summary = u
            .move_with(
                &["a.txt", "b.txt", "c.txt"],
                "/Music",
                Conflict::KeepBoth,
                &cancel,
                |p| {
                    if p.trashing && p.files_done == 1 {
                        stop.cancel();
                    }
                },
            )
            .await
            .unwrap();

        assert!(!u.on_mac("a.txt"), "gone before the cancel");
        assert!(u.on_mac("b.txt") && u.on_mac("c.txt"));
        let removal = summary.removal.unwrap();
        assert_eq!(removal.stopped, Some(Error::Cancelled));
        assert_eq!(removal.left, [u.mac_path("b.txt"), u.mac_path("c.txt")]);
        assert_eq!(removal.removed, 1);
        assert!(
            u.on_phone("Music/a.txt") && u.on_phone("Music/b.txt") && u.on_phone("Music/c.txt"),
            "every copy is on the phone"
        );
    }

    #[tokio::test]
    async fn a_trash_that_refuses_keeps_that_original_and_goes_on() {
        let mut u = up().await;
        u.write("a.txt", b"aaaa");
        u.write("b.txt", b"bbbb");
        let log = Arc::clone(&u.trashed);
        u.service = u
            .service
            .with_trash(recorder(u.bin.path(), &log, |path| path.ends_with("a.txt")));

        let summary = u.mv(&["a.txt", "b.txt"], "/Music").await.unwrap();

        assert!(u.on_mac("a.txt") && !u.on_mac("b.txt"));
        let removal = summary.removal.unwrap();
        assert_eq!(
            removal.kept,
            [Kept {
                label: "a.txt".into(),
                why: Why::Refused(Error::General("Trash is full".into()))
            }]
        );
        assert_eq!(removal.left, [u.mac_path("a.txt")]);
    }

    #[tokio::test]
    async fn retrying_what_was_left_finishes_the_move_without_a_second_copy() {
        // The Trash said no the first time. The retry finds the file already
        // on the phone (same size and date), sends nothing, and now moves it.
        let mut u = up().await;
        u.write("a.txt", b"aaaa");
        let log = Arc::clone(&u.trashed);
        let working = u.service.trash.clone();
        u.service = u.service.with_trash(recorder(u.bin.path(), &log, |_| true));
        let first = u.mv(&["a.txt"], "/Music").await.unwrap();
        let left = first.removal.unwrap().left;
        assert_eq!(left, [u.mac_path("a.txt")]);
        u.service.trash = working;

        let again = u
            .service
            .upload_as(
                Paste::Move,
                &UploadRequest {
                    sources: left,
                    storage_id: u.storage,
                    destination: "/Music".into(),
                },
                Conflict::KeepBoth,
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert!(!u.on_mac("a.txt"), "now it went");
        assert_eq!(again.already_there, 1);
        assert_eq!(again.files, 0, "nothing was sent again");
        let arrival = again.arrival.unwrap();
        assert_eq!(
            (arrival.files, arrival.there, arrival.checked),
            (1, 1, Some(1)),
            "a move reads back, and the file already there counts as checked"
        );
        assert!(u.on_phone("Music/a.txt") && !u.on_phone("Music/a (1).txt"));
        let removal = again.removal.unwrap();
        assert!(removal.left.is_empty());
        assert_eq!(removal.removed, 1);
    }

    #[tokio::test]
    async fn a_folder_merged_into_its_copy_finishes_the_move_without_a_second_one() {
        // What a retry does: the folder the first try made is merged into,
        // not numbered, and nothing of what is there is replaced.
        let mut u = up().await;
        u.write("Trip/1.jpg", b"11");
        u.write("Trip/2.jpg", b"222");
        let log = Arc::clone(&u.trashed);
        let working = u.service.trash.clone();
        u.service = u.service.with_trash(recorder(u.bin.path(), &log, |_| true));
        u.mv(&["Trip"], "/Music").await.unwrap();
        assert!(u.on_mac("Trip"));
        u.service.trash = working;

        let again = u
            .move_with(&["Trip"], "/Music", Conflict::Merge, &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert!(!u.on_mac("Trip"), "now it went");
        assert_eq!((again.already_there, again.merged, again.files), (2, 1, 0));
        assert!(u.on_phone("Music/Trip/2.jpg") && !u.on_phone("Music/Trip (1)"));
        assert_eq!(again.removal.unwrap().removed, 2);
    }

    #[tokio::test]
    async fn a_folder_replaced_into_its_copy_finishes_the_move() {
        // The retry of a folder: its files are on the phone already, so a
        // merge sends nothing and every file counts as checked.
        let mut u = up().await;
        u.write("Trip/1.jpg", b"11");
        u.write("Trip/2.jpg", b"222");
        let log = Arc::clone(&u.trashed);
        let working = u.service.trash.clone();
        u.service = u.service.with_trash(recorder(u.bin.path(), &log, |_| true));
        u.mv(&["Trip"], "/Music").await.unwrap();
        assert!(u.on_mac("Trip"));
        u.service.trash = working;

        let again = u
            .move_with(
                &["Trip"],
                "/Music",
                Conflict::Replace,
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert!(!u.on_mac("Trip"));
        assert_eq!(again.already_there, 2);
        assert!(u.on_phone("Music/Trip/2.jpg") && !u.on_phone("Music/Trip (1)"));
        assert_eq!(again.removal.unwrap().removed, 2);
    }

    #[tokio::test]
    async fn the_move_reports_the_copy_as_a_move_and_then_each_original() {
        let u = up().await;
        u.write("a.txt", b"aaaa");
        u.write("b.txt", b"bbbb");
        let log = Mutex::new(Vec::new());

        u.move_with(
            &["a.txt", "b.txt"],
            "/Music",
            Conflict::KeepBoth,
            &Cancel::new(),
            |p| log.lock().unwrap().push(p),
        )
        .await
        .unwrap();

        let log = log.into_inner().unwrap();
        assert!(log.iter().all(|p| p.moving), "every report is of a move");
        let copying: Vec<_> = log.iter().filter(|p| !p.removing).collect();
        assert!(copying.iter().all(|p| p.direction == Direction::Upload));
        let trashing: Vec<(usize, usize)> = log
            .iter()
            .filter(|p| p.removing && p.trashing)
            .map(|p| (p.files_done, p.files_total))
            .collect();
        assert_eq!(trashing, [(0, 2), (1, 2), (2, 2)]);
    }

    #[tokio::test]
    async fn five_thousand_selected_files_each_get_their_own_trash_call_in_linear_time() {
        // Select all in a camera folder, ⌘X, ⌘V: 5,000 sources, not one folder.
        let u = up().await;
        let names: Vec<String> = (0..5_000).map(|i| format!("{i:05}.jpg")).collect();
        for name in &names {
            u.write(name, b"x");
        }
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let started = std::time::Instant::now();

        let summary = u.mv(&names, "/Music").await.unwrap();

        eprintln!("5,000 selected files moved in {:?}", started.elapsed());
        assert_eq!(summary.files, 5_000);
        assert_eq!(u.trash_calls(), 5_000, "one call per selected item");
        let removal = summary.removal.unwrap();
        assert_eq!(removal.removed, 5_000);
        assert!(removal.left.is_empty() && removal.kept.is_empty());
        assert!(!u.on_mac("04999.jpg") && u.on_phone("Music/04999.jpg"));
    }

    #[tokio::test]
    async fn a_folder_of_five_thousand_files_moves_with_one_trash_call() {
        let u = up().await;
        for i in 0..5_000 {
            u.write(&format!("DCIM/{i:05}.jpg"), b"x");
        }
        let started = std::time::Instant::now();

        let summary = u.mv(&["DCIM"], "/Music").await.unwrap();

        eprintln!("5,000 files moved in {:?}", started.elapsed());
        assert_eq!(summary.files, 5_000);
        assert_eq!(u.trash_calls(), 1, "one call, not one per file");
        assert!(!u.on_mac("DCIM"));
        assert_eq!(summary.removal.unwrap().removed, 5_000);
        let on_phone = fs::read_dir(u.phone.path().join("Music/DCIM"))
            .unwrap()
            .count();
        assert_eq!(on_phone, 5_000);
    }

    fn file(name: &str, size: u64) -> LocalNode {
        LocalNode::for_test(name, false, size)
    }

    fn folder(name: &str, children: Vec<LocalNode>) -> LocalNode {
        LocalNode {
            children,
            ..LocalNode::for_test(name, true, 0)
        }
    }

    #[test]
    fn an_item_that_is_what_it_was_is_the_same() {
        let tree = folder(
            "Trip",
            vec![file("a", 1), folder("day", vec![file("b", 2)])],
        );

        assert_eq!(difference(&tree, &tree.clone()), Difference::Same);
    }

    #[test]
    fn a_file_with_another_size_or_date_has_changed() {
        let was = file("a", 1);
        let mut bigger = was.clone();
        bigger.size = 2;
        let mut later = was.clone();
        later.modified = was.modified.map(|at| at + Duration::from_secs(1));

        assert_eq!(difference(&was, &bigger), Difference::Changed);
        assert_eq!(difference(&was, &later), Difference::Changed);
    }

    #[test]
    fn something_new_in_a_folder_outweighs_a_change() {
        let was = folder("Trip", vec![file("a", 1), file("b", 1)]);
        let new_file = folder("Trip", vec![file("a", 1), file("b", 1), file("c", 1)]);
        let changed = folder("Trip", vec![file("a", 2), file("b", 1)]);
        let both = folder("Trip", vec![file("a", 2), file("b", 1), file("c", 1)]);

        assert_eq!(difference(&was, &new_file), Difference::New);
        assert_eq!(difference(&was, &changed), Difference::Changed);
        assert_eq!(difference(&was, &both), Difference::New);
    }

    #[test]
    fn something_missing_or_of_another_kind_has_changed() {
        let was = folder("Trip", vec![file("a", 1), file("b", 1)]);
        let missing = folder("Trip", vec![file("a", 1)]);
        let swapped = folder("Trip", vec![file("a", 1), file("c", 1)]);
        let kind = folder("Trip", vec![file("a", 1), folder("b", vec![])]);

        assert_eq!(difference(&was, &missing), Difference::Changed);
        // One gone and one new: the new one is what must not be lost.
        assert_eq!(difference(&was, &swapped), Difference::New);
        assert_eq!(difference(&was, &kind), Difference::Changed);
        assert_eq!(
            difference(&file("x", 1), &folder("x", vec![])),
            Difference::Changed
        );
    }

    #[test]
    fn a_change_deep_down_is_seen() {
        let was = folder(
            "a",
            vec![folder("b", vec![folder("c", vec![file("d", 1)])])],
        );
        let now = folder(
            "a",
            vec![folder("b", vec![folder("c", vec![file("d", 9)])])],
        );

        assert_eq!(difference(&was, &now), Difference::Changed);
    }

    #[test]
    fn a_new_entry_that_cannot_be_copied_counts_as_new() {
        let was = folder("Trip", vec![file("a", 1)]);
        let mut now = was.clone();
        now.left_out = 1;

        assert_eq!(difference(&was, &now), Difference::New);
    }

    #[test]
    fn something_left_out_anywhere_below_is_found() {
        let mut deep = folder("c", vec![]);
        deep.left_out = 1;
        let tree = folder("a", vec![folder("b", vec![deep])]);

        assert!(left_out(&tree));
        assert!(!left_out(&folder("a", vec![file("b", 1)])));
    }

    #[test]
    fn comparing_a_big_folder_is_quick() {
        // 5,000 files: the comparison must not be quadratic.
        let files: Vec<LocalNode> = (0..5_000)
            .map(|i| file(&format!("{i:05}.jpg"), 1))
            .collect();
        let was = folder("DCIM", files);
        let started = std::time::Instant::now();

        let same = difference(&was, &was.clone());

        assert_eq!(same, Difference::Same);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn the_check_reads_the_disk_again() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Trip")).unwrap();
        fs::write(dir.path().join("Trip/a.jpg"), b"a").unwrap();
        let mut count = 0;
        let node = scan(dir.path().join("Trip"), "Trip".into(), 0, &mut count)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(may_go(&node).await, Ok(()));

        fs::write(dir.path().join("Trip/b.jpg"), b"b").unwrap();
        assert_eq!(may_go(&node).await, Err(Why::NotEmpty));

        fs::remove_file(dir.path().join("Trip/b.jpg")).unwrap();
        fs::remove_file(dir.path().join("Trip/a.jpg")).unwrap();
        assert_eq!(may_go(&node).await, Err(Why::Changed));

        fs::remove_dir(dir.path().join("Trip")).unwrap();
        assert_eq!(may_go(&node).await, Err(Why::Changed), "gone altogether");
    }
}
