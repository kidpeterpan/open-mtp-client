//! Copying and moving inside the Mac, as Finder does it: nothing is ever
//! overwritten (a taken name gets ` (1)`, ` (2)`, ...), a folder can't go into
//! itself, and a move inside one volume is a rename.
//!
//! `std::fs::copy` is the copy: on APFS it clones the file (600 MB in about
//! 100 µs), keeps extended attributes and the modified date, and on other
//! volumes it falls back to copying the data. Files are copied to a hidden
//! part file and put in place with a hard link, which never replaces anything
//! (the check for a free name and the placing are not the same moment, so the
//! name is decided early and defended at the end).
//!
//! The pane does what is quick by itself (`Service::paste`): moves inside a
//! volume, and copies of up to `INLINE_BYTES`. Anything bigger is for the
//! transfer queue (`paste_queued`), which reports progress and can be
//! cancelled; the pane hands it over by saying `Unsupported`, as a phone that
//! can't copy by itself does. Progress is a file at a time: one big file on a
//! volume that can't clone goes from nothing to done.
//!
//! A move to another volume is a copy; each item's original goes to the Trash
//! only once all of it has arrived at the size it had, and an item with
//! anything in it that could not be copied (a socket, say) stays whole.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::Service;
use super::mapper;
use crate::dto::{
    Cancel, Direction, Error, FileFailure, Kept, Paste, PasteRequest, PasteSummary, Removal,
    TransferProgress, TransferSummary, Why,
};
use crate::service::names;

/// Copies of up to this much are done where they were asked for (they take a
/// moment even without cloning); more goes to the transfer queue.
pub(super) const INLINE_BYTES: u64 = 256 * 1024 * 1024;
/// ... and of up to this many files, which are one request each.
pub(super) const INLINE_FILES: usize = 5_000;
/// The same bounds as a copy from the phone: a tree deeper or bigger than this
/// is a mistake, or a trap.
const MAX_DEPTH: usize = 64;
const MAX_ITEMS: usize = 1_000_000;
/// Tries at a free name when something else takes it between choosing and
/// placing.
const NAME_TRIES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// A folder to make, relative to the item's target.
    Folder(PathBuf),
    File {
        relative: PathBuf,
        size: u64,
    },
    Link(PathBuf),
}

/// One selected item, as it will be done.
#[derive(Debug)]
struct Item {
    /// As it was selected.
    name: String,
    source: PathBuf,
    /// What it is called where it goes: its own name if that is free.
    target_name: String,
    is_folder: bool,
    /// What is in it, for a copy. Empty for a rename.
    steps: Vec<Step>,
    /// Things in it that can't be copied (sockets, pipes, devices).
    left_out: usize,
}

/// What a paste will do, found out before anything is touched.
#[derive(Debug)]
struct Plan {
    items: Vec<Item>,
    into: PathBuf,
    /// A move inside one volume: a rename, no copying.
    rename: bool,
    files: usize,
    bytes: u64,
    /// Nothing to do: a move into the folder the items are already in.
    nothing: bool,
}

/// The names in a folder, ignoring case as the Mac's file system does, and the
/// ones given out since.
struct Taken(HashSet<String>);

impl Taken {
    fn of(folder: &Path) -> io::Result<Self> {
        let mut taken = HashSet::new();
        for entry in fs::read_dir(folder)? {
            taken.insert(entry?.file_name().to_string_lossy().to_lowercase());
        }
        Ok(Self(taken))
    }

    /// A name for something new called `name`: itself if free, else the first
    /// numbered one. It is taken from now on.
    fn claim(&mut self, name: &str, is_folder: bool) -> String {
        let chosen = if self.0.contains(&name.to_lowercase()) {
            (1..)
                .map(|n| names::numbered(name, n, is_folder))
                .find(|candidate| !self.0.contains(&candidate.to_lowercase()))
                .unwrap_or_else(|| name.to_owned())
        } else {
            name.to_owned()
        };
        self.0.insert(chosen.to_lowercase());
        chosen
    }
}

impl Service {
    /// Whether a move inside one volume may be a rename (a test can say no).
    fn can_rename(&self) -> bool {
        #[cfg(test)]
        {
            !self.copy_moves
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    /// Copies or moves `request.names` from the folder `request.from` into the
    /// folder `request.to`, for the pane: what is quick is done now, and what
    /// is not is left for the queue (`Error::Unsupported`, with nothing
    /// changed). A name that is taken is never replaced.
    pub async fn paste(&self, request: &PasteRequest) -> Result<PasteSummary, Error> {
        let plan = plan(request.clone(), self.can_rename()).await?;
        if plan.nothing {
            return Ok(summary_of(request.mode, &plan, &[], Vec::new()));
        }
        if !plan.rename && (plan.bytes > INLINE_BYTES || plan.files > INLINE_FILES) {
            return Err(Error::Unsupported);
        }
        let cancel = Cancel::new();
        let done = self.run(request.mode, &plan, &cancel, &|_| {}).await?;
        Ok(summary_of(
            request.mode,
            &plan,
            &done.finished,
            done.failures,
        ))
    }

    /// `paste`, for what the pane handed over: any size, with progress and a
    /// way to stop. A move to another volume puts each original in the Trash
    /// once its copy is whole; the summary says what stayed.
    pub async fn paste_queued(
        &self,
        request: &PasteRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let plan = plan(request.clone(), self.can_rename()).await?;
        let done = self.run(request.mode, &plan, cancel, &progress).await?;
        let (files, bytes) = (done.files, done.bytes);
        let mut summary = TransferSummary {
            direction: Direction::Local,
            files,
            bytes,
            renamed: done
                .finished
                .iter()
                .filter(|&&index| plan.items[index].target_name != plan.items[index].name)
                .count(),
            failures: done.failures,
            destination: request.to.clone(),
            first_item: done
                .finished
                .first()
                .map(|&index| plan.items[index].target_name.clone()),
            ..TransferSummary::default()
        };
        if request.mode == Paste::Move && !plan.rename {
            summary.removal = Some(done.removal);
        } else if request.mode == Paste::Move {
            // A rename moved the items themselves: nothing was copied, nothing is
            // left behind.
            summary.files = done.finished.len();
            summary.removal = Some(Removal {
                from_mac: true,
                removed: done.finished.len(),
                ..Removal::default()
            });
        }
        Ok(summary)
    }

    async fn run(
        &self,
        mode: Paste,
        plan: &Plan,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) -> Result<Done, Error> {
        let mut done = Done::default();
        if plan.rename {
            for (index, item) in plan.items.iter().enumerate() {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                match place_move(&item.source, &plan.into, item).await {
                    Ok(final_name) => {
                        // (The name may have had to change at the last moment.)
                        done.finished.push(index);
                        done.names.push(final_name);
                    }
                    Err(err) => done.fail(item, &err, &plan.into),
                }
            }
            return Ok(done);
        }

        let moving = mode == Paste::Move;
        let started = Instant::now();
        let mut state = TransferProgress {
            direction: Direction::Local,
            moving,
            files_total: plan.files,
            bytes_total: plan.bytes,
            ..TransferProgress::default()
        };
        progress(state.clone());
        for (index, item) in plan.items.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if item.steps.is_empty() {
                // A socket, a pipe, a device: nothing a copy can make of it.
                done.failures.push(FileFailure {
                    label: item.name.clone(),
                    error: Error::General("it is not a file or a folder".to_owned()),
                });
                continue;
            }
            let copied = copy_item(item, &plan.into, cancel, &mut state, started, progress).await;
            match copied {
                Ok(copy) => {
                    done.files += copy.files;
                    done.bytes += copy.bytes;
                    done.finished.push(index);
                    done.names.push(copy.final_name);
                }
                Err(Stop::Cancelled) => return Err(Error::Cancelled),
                Err(Stop::Failed(err)) => {
                    let stops = matches!(mapper::to_error(&err, ""), Error::MacDiskFull);
                    done.fail(item, &err, &plan.into);
                    if stops {
                        return Err(Error::MacDiskFull);
                    }
                }
            }
        }
        if moving {
            self.remove_originals(plan, &mut done, cancel, progress)
                .await;
        }
        state.elapsed = started.elapsed();
        progress(state);
        Ok(done)
    }

    /// A move to another volume: each original goes to the Trash once its
    /// whole copy is there at the size it had.
    async fn remove_originals(
        &self,
        plan: &Plan,
        done: &mut Done,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) {
        let candidates: Vec<usize> = done.finished.clone();
        let report = |count: usize| TransferProgress {
            direction: Direction::Local,
            moving: true,
            removing: true,
            trashing: true,
            files_done: count,
            files_total: candidates.len(),
            ..TransferProgress::default()
        };
        done.removal.from_mac = true;
        // What failed to copy is still where it was, for a retry.
        done.removal
            .left
            .extend(done.failures.iter().map(|failure| failure.label.clone()));
        progress(report(0));
        let mut gone = 0;
        for (turn, &index) in candidates.iter().enumerate() {
            let item = &plan.items[index];
            if cancel.is_cancelled() {
                done.removal.stopped = Some(Error::Cancelled);
                done.removal.left.extend(
                    candidates[turn..]
                        .iter()
                        .map(|&later| plan.items[later].name.clone()),
                );
                return;
            }
            let target = plan.into.join(&done.names[turn]);
            let why = if item.left_out > 0 {
                Some(Why::NotCopied)
            } else if !arrived_whole(item, &target).await {
                Some(Why::Unchecked)
            } else {
                None
            };
            let result = match why {
                Some(why) => Err(why),
                None => {
                    let source = item.source.clone();
                    self.trash_off_thread(&source).await.map_err(|err| {
                        Why::Refused(mapper::to_error(&err, &source.to_string_lossy()))
                    })
                }
            };
            match result {
                Ok(()) => {
                    done.removal.removed += item
                        .steps
                        .iter()
                        .filter(|step| matches!(step, Step::File { .. }))
                        .count()
                        .max(1);
                    gone += 1;
                    progress(report(gone));
                }
                Err(why) => {
                    done.removal.kept.push(Kept {
                        label: item.name.clone(),
                        why,
                    });
                    done.removal.left.push(item.name.clone());
                }
            }
        }
    }
}

/// What a paste did.
#[derive(Default)]
struct Done {
    /// The items that went, by their place in the plan, and what each is called
    /// now.
    finished: Vec<usize>,
    names: Vec<String>,
    failures: Vec<FileFailure>,
    files: usize,
    bytes: u64,
    removal: Removal,
}

impl Done {
    fn fail(&mut self, item: &Item, err: &io::Error, into: &Path) {
        self.failures.push(FileFailure {
            label: item.name.clone(),
            error: mapper::to_error(err, &into.join(&item.target_name).to_string_lossy()),
        });
    }
}

fn summary_of(
    mode: Paste,
    plan: &Plan,
    finished: &[usize],
    failures: Vec<FileFailure>,
) -> PasteSummary {
    let names: Vec<String> = finished
        .iter()
        .map(|&index| plan.items[index].target_name.clone())
        .collect();
    PasteSummary {
        mode,
        done: finished.len(),
        renamed: finished
            .iter()
            .filter(|&&index| plan.items[index].target_name != plan.items[index].name)
            .count(),
        into: plan.into.to_string_lossy().into_owned(),
        names,
        failures,
    }
}

/// Why a copy of one item ended before the end.
enum Stop {
    Cancelled,
    Failed(io::Error),
}

impl From<io::Error> for Stop {
    fn from(err: io::Error) -> Self {
        Stop::Failed(err)
    }
}

struct Copied {
    final_name: String,
    files: usize,
    bytes: u64,
}

/// Works out what a paste will do. Looks only: nothing is made or moved.
async fn plan(request: PasteRequest, can_rename: bool) -> Result<Plan, Error> {
    tokio::task::spawn_blocking(move || plan_now(&request, can_rename))
        .await
        .map_err(|err| Error::General(err.to_string()))?
}

fn plan_now(request: &PasteRequest, can_rename: bool) -> Result<Plan, Error> {
    let (from, into) = (PathBuf::from(&request.from), PathBuf::from(&request.to));
    let folder_error =
        |path: &Path, err: &io::Error| mapper::to_error(err, &path.to_string_lossy());
    let from_meta = fs::metadata(&from).map_err(|err| folder_error(&from, &err))?;
    let into_meta = fs::metadata(&into).map_err(|err| folder_error(&into, &err))?;
    if !from_meta.is_dir() {
        return Err(Error::PathNotFound(request.from.clone()));
    }
    if !into_meta.is_dir() {
        return Err(Error::PathNotFound(request.to.clone()));
    }
    let real_into = fs::canonicalize(&into).map_err(|err| folder_error(&into, &err))?;
    let real_from = fs::canonicalize(&from).map_err(|err| folder_error(&from, &err))?;
    let moving = request.mode == Paste::Move;
    let nothing = moving && real_from == real_into;
    let rename = moving && can_rename && from_meta.dev() == into_meta.dev();

    let mut taken = Taken::of(&into).map_err(|err| folder_error(&into, &err))?;
    let mut items = Vec::with_capacity(request.names.len());
    let (mut files, mut bytes, mut seen) = (0, 0, 0);
    for name in &request.names {
        if !names::is_safe_component(name) {
            return Err(Error::UnsafeName(name.clone()));
        }
        let source = from.join(name);
        let meta = fs::symlink_metadata(&source).map_err(|err| folder_error(&source, &err))?;
        let is_folder = meta.is_dir();
        // A folder can't go into itself, or into anything inside itself.
        if is_folder && real_into.starts_with(real_from.join(name)) {
            return Err(Error::IntoItself(name.clone()));
        }
        let mut steps = Vec::new();
        let mut left_out = 0;
        if !rename && !nothing {
            walk(
                &source,
                Path::new(""),
                0,
                &mut steps,
                &mut left_out,
                &mut seen,
            )?;
            files += steps
                .iter()
                .filter(|step| matches!(step, Step::File { .. }))
                .count();
            bytes += steps
                .iter()
                .map(|step| match step {
                    Step::File { size, .. } => *size,
                    _ => 0,
                })
                .sum::<u64>();
        }
        let target_name = if nothing {
            name.clone()
        } else {
            taken.claim(name, is_folder)
        };
        items.push(Item {
            name: name.clone(),
            source,
            target_name,
            is_folder,
            steps,
            left_out,
        });
    }
    Ok(Plan {
        items,
        into,
        rename,
        files,
        bytes,
        nothing,
    })
}

/// What is in `path`, as steps relative to the item it is part of. A folder
/// comes before what is in it; a link is a link, never followed.
fn walk(
    path: &Path,
    relative: &Path,
    depth: usize,
    steps: &mut Vec<Step>,
    left_out: &mut usize,
    seen: &mut usize,
) -> Result<(), Error> {
    if depth > MAX_DEPTH || *seen >= MAX_ITEMS {
        return Err(Error::TransferTooLarge);
    }
    *seen += 1;
    let error = |err: io::Error| mapper::to_error(&err, &path.to_string_lossy());
    let meta = fs::symlink_metadata(path).map_err(error)?;
    if meta.is_symlink() {
        steps.push(Step::Link(relative.to_path_buf()));
    } else if meta.is_dir() {
        steps.push(Step::Folder(relative.to_path_buf()));
        for entry in fs::read_dir(path).map_err(error)? {
            let entry = entry.map_err(error)?;
            let child = relative.join(entry.file_name());
            walk(&entry.path(), &child, depth + 1, steps, left_out, seen)?;
        }
    } else if meta.is_file() {
        steps.push(Step::File {
            relative: relative.to_path_buf(),
            size: meta.len(),
        });
    } else {
        *left_out += 1;
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

/// Moves one item into `into` by renaming it, under its planned name or, if
/// something took that name since, the next free one. Never replaces.
async fn place_move(source: &Path, into: &Path, item: &Item) -> io::Result<String> {
    let (source, into, item_name, is_folder) = (
        source.to_path_buf(),
        into.to_path_buf(),
        item.target_name.clone(),
        item.is_folder,
    );
    blocking(move || {
        for attempt in 0..NAME_TRIES {
            let name = if attempt == 0 {
                item_name.clone()
            } else {
                names::numbered(&item_name, attempt, is_folder)
            };
            match move_without_replacing(&source, &into.join(&name)) {
                Ok(()) => return Ok(name),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::from(io::ErrorKind::AlreadyExists))
    })
    .await
}

/// A rename that leaves what is at `to` alone. A plain file gets a hard link
/// (which fails if the name is there) and loses its old name; anything else is
/// checked and renamed, since a rename over a name that appeared in between
/// could only replace an empty folder.
fn move_without_replacing(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_file() {
        match fs::hard_link(from, to) {
            Ok(()) => return fs::remove_file(from),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(err),
            // A file system without links: the check and the rename below.
            Err(_) => {}
        }
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    fs::rename(from, to)
}

/// Copies one item. Progress is a file at a time, and a cancel is looked at
/// between files.
async fn copy_item(
    item: &Item,
    into: &Path,
    cancel: &Cancel,
    state: &mut TransferProgress,
    started: Instant,
    progress: &(impl Fn(TransferProgress) + Send + Sync),
) -> Result<Copied, Stop> {
    // The item itself gets its name now; what is inside has nowhere to clash.
    let mut final_name = item.target_name.clone();
    let mut target = into.join(&final_name);
    let (mut files, mut bytes) = (0, 0);
    for step in &item.steps {
        if cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        match step {
            Step::Folder(relative) => {
                let relative = relative.clone();
                if relative.as_os_str().is_empty() {
                    let (parent, name, is_folder) = (into.to_path_buf(), final_name.clone(), true);
                    final_name = blocking(move || make_folder(&parent, &name, is_folder)).await?;
                    target = into.join(&final_name);
                } else {
                    let made = target.join(&relative);
                    blocking(move || fs::create_dir(made)).await?;
                }
            }
            Step::Link(relative) => {
                let source = if relative.as_os_str().is_empty() {
                    item.source.clone()
                } else {
                    item.source.join(relative)
                };
                let (parent, name) = (into.to_path_buf(), final_name.clone());
                if relative.as_os_str().is_empty() {
                    final_name =
                        blocking(move || copy_link_at_top(&source, &parent, &name)).await?;
                    target = into.join(&final_name);
                } else {
                    let made = target.join(relative);
                    blocking(move || copy_link(&source, &made)).await?;
                }
            }
            Step::File { relative, size } => {
                let top = relative.as_os_str().is_empty();
                let source = if top {
                    item.source.clone()
                } else {
                    item.source.join(relative)
                };
                state.current_file = if top {
                    item.name.clone()
                } else {
                    format!("{}/{}", item.name, relative.to_string_lossy())
                };
                state.file_bytes_total = *size;
                state.file_bytes_done = 0;
                state.elapsed = started.elapsed();
                progress(state.clone());
                let (parent, name) = if top {
                    (into.to_path_buf(), final_name.clone())
                } else {
                    let place = target.join(relative);
                    (
                        place.parent().map(Path::to_path_buf).unwrap_or_default(),
                        place
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    )
                };
                let placed = blocking(move || copy_file(&source, &parent, &name, top)).await?;
                if top {
                    final_name = placed;
                    target = into.join(&final_name);
                }
                files += 1;
                bytes += size;
                state.files_done += 1;
                state.bytes_done += size;
                state.file_bytes_done = *size;
                state.elapsed = started.elapsed();
                progress(state.clone());
            }
        }
    }
    Ok(Copied {
        final_name,
        files,
        bytes,
    })
}

/// A new folder called `name` in `parent`, or called the next free number if
/// something took the name since it was chosen.
fn make_folder(parent: &Path, name: &str, is_folder: bool) -> io::Result<String> {
    for attempt in 0..NAME_TRIES {
        let candidate = if attempt == 0 {
            name.to_owned()
        } else {
            names::numbered(name, attempt, is_folder)
        };
        match fs::create_dir(parent.join(&candidate)) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

fn copy_link(source: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, to)
}

/// A link copied to `parent/name`, or to the next free number.
fn copy_link_at_top(source: &Path, parent: &Path, name: &str) -> io::Result<String> {
    for attempt in 0..NAME_TRIES {
        let candidate = if attempt == 0 {
            name.to_owned()
        } else {
            names::numbered(name, attempt, false)
        };
        match copy_link(source, &parent.join(&candidate)) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// Copies a file to `parent/name` through a hidden part file, put in place
/// without replacing anything. A top-level file whose name has been taken
/// since it was chosen (`renumber`) goes under the next free number; the name
/// it ended up with is returned.
fn copy_file(source: &Path, parent: &Path, name: &str, renumber: bool) -> io::Result<String> {
    let part = parent.join(format!(".{name}.open-mtp-client-part"));
    let _ = fs::remove_file(&part);
    let staged = fs::copy(source, &part).and_then(|_| {
        // Whatever the volume, the copy has the date of the original.
        let when = fs::metadata(source)?.modified()?;
        fs::File::options()
            .write(true)
            .open(&part)?
            .set_modified(when)
    });
    if let Err(err) = staged {
        let _ = fs::remove_file(&part);
        return Err(err);
    }
    let tries = if renumber { NAME_TRIES } else { 1 };
    for attempt in 0..tries {
        let candidate = if attempt == 0 {
            name.to_owned()
        } else {
            names::numbered(name, attempt, false)
        };
        match place(&part, &parent.join(&candidate)) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                let _ = fs::remove_file(&part);
                return Err(err);
            }
        }
    }
    let _ = fs::remove_file(&part);
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// Puts the finished `part` at `to`, never replacing what is there.
fn place(part: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(part, to) {
        Ok(()) => fs::remove_file(part),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(err),
        // A file system without links.
        Err(_) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs::rename(part, to)
        }
    }
}

/// Whether everything of `item` is at `target` at the size it had.
async fn arrived_whole(item: &Item, target: &Path) -> bool {
    let (item_steps, source, target) = (
        item.steps.clone(),
        item.source.clone(),
        target.to_path_buf(),
    );
    blocking(move || {
        for step in &item_steps {
            match step {
                Step::File { relative, size } => {
                    let at = if relative.as_os_str().is_empty() {
                        target.clone()
                    } else {
                        target.join(relative)
                    };
                    if fs::symlink_metadata(&at)?.len() != *size {
                        return Ok(false);
                    }
                }
                Step::Folder(relative) => {
                    let at = target.join(relative);
                    if !fs::symlink_metadata(&at)?.is_dir() {
                        return Ok(false);
                    }
                }
                Step::Link(relative) => {
                    let at = target.join(relative);
                    if fs::read_link(&at)?
                        != fs::read_link(if relative.as_os_str().is_empty() {
                            source.clone()
                        } else {
                            source.join(relative)
                        })?
                    {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    /// A Mac with `Documents/` and `Backup/` in a home of its own, and a Trash
    /// that is a folder of the test's (never the user's).
    struct Mac {
        home: tempfile::TempDir,
        bin: tempfile::TempDir,
        trashed: Arc<Mutex<Vec<PathBuf>>>,
    }

    fn mac() -> Mac {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("Documents")).unwrap();
        fs::create_dir_all(home.path().join("Backup")).unwrap();
        Mac {
            home,
            bin: tempfile::tempdir().unwrap(),
            trashed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    impl Mac {
        fn service(&self) -> Service {
            self.service_refusing(|_| false)
        }

        fn service_refusing(
            &self,
            refuse: impl Fn(&Path) -> bool + Send + Sync + 'static,
        ) -> Service {
            let (bin, log) = (self.bin.path().to_path_buf(), Arc::clone(&self.trashed));
            let count = AtomicUsize::new(0);
            Service::with_trash(
                self.home.path().to_path_buf(),
                std::sync::Arc::new(move |path: &std::path::Path| {
                    if refuse(path) {
                        return Err(io::Error::other("Trash is full"));
                    }
                    log.lock().unwrap().push(path.to_path_buf());
                    let n = count.fetch_add(1, Ordering::Relaxed);
                    fs::rename(
                        path,
                        bin.join(format!(
                            "{n}-{}",
                            path.file_name().unwrap().to_string_lossy()
                        )),
                    )
                }),
            )
        }

        /// A service whose moves all cross a volume.
        fn crossing(&self) -> Service {
            let mut service = self.service();
            service.copy_moves = true;
            service
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.home.path().join(relative)
        }

        fn write(&self, relative: &str, content: &[u8]) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn read(&self, relative: &str) -> Vec<u8> {
            fs::read(self.path(relative)).unwrap()
        }

        fn exists(&self, relative: &str) -> bool {
            fs::symlink_metadata(self.path(relative)).is_ok()
        }

        fn names(&self, folder: &str) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.path(folder))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        fn request(&self, mode: Paste, from: &str, names: &[&str], to: &str) -> PasteRequest {
            PasteRequest {
                mode,
                from_storage: 0,
                from: self.path(from).to_string_lossy().into_owned(),
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                to_storage: 0,
                to: self.path(to).to_string_lossy().into_owned(),
            }
        }

        fn trash_calls(&self) -> usize {
            self.trashed.lock().unwrap().len()
        }
    }

    fn date(secs: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn set_date(path: &Path, secs: u64) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(date(secs))
            .unwrap();
    }

    fn modified(path: &Path) -> std::time::SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[tokio::test]
    async fn a_file_is_copied_with_its_date_and_the_original_stays() {
        let m = mac();
        m.write("Documents/a.txt", b"hello");
        set_date(&m.path("Documents/a.txt"), 1_600_000_000);

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["a.txt"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.read("Backup/a.txt"), b"hello");
        assert_eq!(m.read("Documents/a.txt"), b"hello");
        assert_eq!(modified(&m.path("Backup/a.txt")), date(1_600_000_000));
        assert_eq!((summary.done, summary.renamed), (1, 0));
        assert_eq!(summary.names, ["a.txt"]);
        assert_eq!(m.names("Backup"), ["a.txt"], "no part file left");
    }

    #[tokio::test]
    async fn a_folder_is_copied_with_everything_in_it() {
        let m = mac();
        m.write("Documents/Trip/1.jpg", b"11");
        m.write("Documents/Trip/day2/2.jpg", b"222");
        fs::create_dir_all(m.path("Documents/Trip/Empty")).unwrap();
        std::os::unix::fs::symlink("1.jpg", m.path("Documents/Trip/link")).unwrap();
        set_date(&m.path("Documents/Trip/1.jpg"), 1_500_000_000);

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["Trip"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.read("Backup/Trip/day2/2.jpg"), b"222");
        assert!(m.path("Backup/Trip/Empty").is_dir());
        assert_eq!(
            fs::read_link(m.path("Backup/Trip/link")).unwrap(),
            Path::new("1.jpg"),
            "a link is copied as a link, never followed"
        );
        assert_eq!(modified(&m.path("Backup/Trip/1.jpg")), date(1_500_000_000));
        assert!(m.exists("Documents/Trip/1.jpg"));
        assert_eq!(summary.done, 1);
    }

    #[tokio::test]
    async fn a_taken_name_is_never_replaced() {
        let m = mac();
        m.write("Documents/a.txt", b"new");
        m.write("Backup/a.txt", b"mine");
        m.write("Backup/a (1).txt", b"also mine");

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["a.txt"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.read("Backup/a.txt"), b"mine");
        assert_eq!(m.read("Backup/a (1).txt"), b"also mine");
        assert_eq!(m.read("Backup/a (2).txt"), b"new");
        assert_eq!((summary.done, summary.renamed), (1, 1));
        assert_eq!(summary.names, ["a (2).txt"], "what it is called now");
    }

    #[tokio::test]
    async fn a_name_is_taken_whatever_its_case_as_on_the_macs_own_file_system() {
        let m = mac();
        m.write("Documents/Photo.jpg", b"new");
        m.write("Backup/photo.JPG", b"mine");

        m.service()
            .paste(&m.request(Paste::Copy, "Documents", &["Photo.jpg"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.read("Backup/photo.JPG"), b"mine");
        assert_eq!(m.read("Backup/Photo (1).jpg"), b"new");
    }

    #[tokio::test]
    async fn copying_into_the_folder_the_item_is_in_makes_a_numbered_copy() {
        let m = mac();
        m.write("Documents/a.txt", b"hello");

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["a.txt"], "Documents"))
            .await
            .unwrap();

        assert_eq!(m.names("Documents"), ["a (1).txt", "a.txt"]);
        assert_eq!(summary.renamed, 1);
    }

    #[tokio::test]
    async fn several_items_get_names_that_do_not_clash_with_each_other() {
        let m = mac();
        m.write("Documents/a.txt", b"1");
        m.write("Documents/b.txt", b"2");
        m.write("Backup/a.txt", b"mine");

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["a.txt", "b.txt"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.names("Backup"), ["a (1).txt", "a.txt", "b.txt"]);
        assert_eq!(summary.names, ["a (1).txt", "b.txt"]);
    }

    #[tokio::test]
    async fn a_move_inside_a_volume_is_a_rename_that_keeps_the_file_itself() {
        let m = mac();
        m.write("Documents/Trip/1.jpg", b"11");
        m.write("Documents/a.txt", b"hello");
        let before = (
            fs::metadata(m.path("Documents/a.txt")).unwrap().ino(),
            fs::metadata(m.path("Documents/Trip")).unwrap().ino(),
        );

        let summary = m
            .service()
            .paste(&m.request(Paste::Move, "Documents", &["a.txt", "Trip"], "Backup"))
            .await
            .unwrap();

        assert!(!m.exists("Documents/a.txt") && !m.exists("Documents/Trip"));
        assert_eq!(m.read("Backup/Trip/1.jpg"), b"11");
        let after = (
            fs::metadata(m.path("Backup/a.txt")).unwrap().ino(),
            fs::metadata(m.path("Backup/Trip")).unwrap().ino(),
        );
        assert_eq!(before, after, "the same file and folder, not copies");
        assert_eq!(summary.done, 2);
        assert_eq!(m.trash_calls(), 0);
    }

    #[tokio::test]
    async fn a_move_never_replaces_either() {
        let m = mac();
        m.write("Documents/a.txt", b"new");
        m.write("Documents/Trip/1.jpg", b"11");
        m.write("Backup/a.txt", b"mine");
        m.write("Backup/Trip/other.jpg", b"theirs");

        let summary = m
            .service()
            .paste(&m.request(Paste::Move, "Documents", &["a.txt", "Trip"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.read("Backup/a.txt"), b"mine");
        assert_eq!(m.read("Backup/a (1).txt"), b"new");
        assert_eq!(m.names("Backup/Trip"), ["other.jpg"], "left alone");
        assert_eq!(m.read("Backup/Trip (1)/1.jpg"), b"11");
        assert_eq!(summary.renamed, 2);
    }

    #[tokio::test]
    async fn a_move_into_the_folder_the_item_is_in_does_nothing() {
        let m = mac();
        m.write("Documents/a.txt", b"hello");

        let summary = m
            .service()
            .paste(&m.request(Paste::Move, "Documents", &["a.txt"], "Documents"))
            .await
            .unwrap();

        assert_eq!(summary.done, 0);
        assert_eq!(m.names("Documents"), ["a.txt"]);
    }

    #[tokio::test]
    async fn a_folder_cannot_go_into_itself_or_into_anything_in_it() {
        let m = mac();
        m.write("Documents/Trip/day1/1.jpg", b"1");
        for mode in [Paste::Copy, Paste::Move] {
            for to in ["Documents/Trip", "Documents/Trip/day1"] {
                let result = m
                    .service()
                    .paste(&m.request(mode, "Documents", &["Trip"], to))
                    .await;

                assert_eq!(
                    result.err(),
                    Some(Error::IntoItself("Trip".into())),
                    "{mode:?} into {to}"
                );
            }
        }
        assert_eq!(m.names("Documents/Trip"), ["day1"], "nothing was touched");
    }

    #[tokio::test]
    async fn a_folder_cannot_go_into_itself_through_a_link_either() {
        let m = mac();
        m.write("Documents/Trip/1.jpg", b"1");
        std::os::unix::fs::symlink(m.path("Documents/Trip"), m.path("Backup/shortcut")).unwrap();

        let result = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["Trip"], "Backup/shortcut"))
            .await;

        assert_eq!(result.err(), Some(Error::IntoItself("Trip".into())));
    }

    #[tokio::test]
    async fn names_that_are_not_one_plain_component_are_refused() {
        let m = mac();
        m.write("Documents/a.txt", b"1");
        for name in ["../Backup", "x/y", "..", ""] {
            let result = m
                .service()
                .paste(&m.request(Paste::Copy, "Documents", &[name], "Backup"))
                .await;

            assert_eq!(
                result.err(),
                Some(Error::UnsafeName(name.into())),
                "{name:?}"
            );
        }
    }

    #[tokio::test]
    async fn what_is_not_there_is_not_found_and_nothing_is_done() {
        let m = mac();
        m.write("Documents/a.txt", b"1");

        let missing_item = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["a.txt", "ghost"], "Backup"))
            .await;
        let missing_folder = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["a.txt"], "Nowhere"))
            .await;

        assert!(matches!(missing_item, Err(Error::PathNotFound(_))));
        assert!(matches!(missing_folder, Err(Error::PathNotFound(_))));
        assert!(m.names("Backup").is_empty(), "a.txt was not half done");
    }

    #[tokio::test]
    async fn an_item_that_fails_is_that_items_failure_and_the_rest_go_on() {
        let m = mac();
        m.write("Documents/a.txt", b"1");
        m.write("Documents/locked.txt", b"secret");
        m.write("Documents/c.txt", b"3");
        let locked = m.path("Documents/locked.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let summary = m
            .service()
            .paste(&m.request(
                Paste::Copy,
                "Documents",
                &["a.txt", "locked.txt", "c.txt"],
                "Backup",
            ))
            .await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        let summary = summary.unwrap();

        assert_eq!(m.names("Backup"), ["a.txt", "c.txt"], "and no part file");
        assert_eq!(summary.done, 2);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].label, "locked.txt");
    }

    #[tokio::test]
    async fn a_copy_too_big_for_the_pane_is_left_for_the_queue_with_nothing_changed() {
        let m = mac();
        let big = fs::File::create(m.path("Documents/movie.mov")).unwrap();
        big.set_len(INLINE_BYTES + 1).unwrap();

        let result = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["movie.mov"], "Backup"))
            .await;

        assert_eq!(result.err(), Some(Error::Unsupported));
        assert!(m.names("Backup").is_empty());
    }

    #[tokio::test]
    async fn a_copy_of_too_many_files_is_left_for_the_queue_too() {
        let m = mac();
        fs::create_dir_all(m.path("Documents/Many")).unwrap();
        for i in 0..=INLINE_FILES {
            fs::write(m.path(&format!("Documents/Many/{i}.txt")), b"x").unwrap();
        }

        let result = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["Many"], "Backup"))
            .await;

        assert_eq!(result.err(), Some(Error::Unsupported));
        assert!(m.names("Backup").is_empty());
    }

    #[tokio::test]
    async fn a_move_inside_a_volume_is_never_left_for_the_queue_however_big() {
        let m = mac();
        let big = fs::File::create(m.path("Documents/movie.mov")).unwrap();
        big.set_len(INLINE_BYTES * 20).unwrap();

        let summary = m
            .service()
            .paste(&m.request(Paste::Move, "Documents", &["movie.mov"], "Backup"))
            .await
            .unwrap();

        assert_eq!(summary.done, 1);
        assert!(m.exists("Backup/movie.mov") && !m.exists("Documents/movie.mov"));
    }

    // ---- the queue: any size, with progress and a way to stop ----

    fn reports() -> (
        Arc<Mutex<Vec<TransferProgress>>>,
        impl Fn(TransferProgress) + Send + Sync,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&log);
        (log, move |report| writer.lock().unwrap().push(report))
    }

    #[tokio::test]
    async fn a_queued_copy_reports_a_file_at_a_time_and_ends_full() {
        let m = mac();
        m.write("Documents/Trip/a.jpg", b"aaaa");
        m.write("Documents/Trip/day2/b.jpg", b"bb");
        let (log, progress) = reports();

        let summary = m
            .service()
            .paste_queued(
                &m.request(Paste::Copy, "Documents", &["Trip"], "Backup"),
                &Cancel::new(),
                progress,
            )
            .await
            .unwrap();

        let log = log.lock().unwrap();
        assert!(
            log.iter()
                .all(|r| r.direction == Direction::Local && !r.moving)
        );
        assert!(log.iter().all(|r| r.files_total == 2 && r.bytes_total == 6));
        assert_eq!(log.last().unwrap().files_done, 2);
        assert_eq!(log.last().unwrap().bytes_done, 6);
        assert!(
            log.windows(2)
                .all(|pair| pair[0].bytes_done <= pair[1].bytes_done)
        );
        assert!(log.iter().any(|r| r.current_file == "Trip/day2/b.jpg"));
        assert_eq!((summary.files, summary.bytes), (2, 6));
        assert_eq!(summary.direction, Direction::Local);
        assert_eq!(summary.first_item.as_deref(), Some("Trip"));
        assert_eq!(summary.destination, m.path("Backup").to_string_lossy());
        assert_eq!(summary.removal, None);
    }

    #[tokio::test]
    async fn cancelling_a_queued_copy_stops_between_files_and_leaves_no_part_file() {
        let m = mac();
        for i in 0..5 {
            m.write(&format!("Documents/Trip/{i}.txt"), b"x");
        }
        let cancel = Cancel::new();
        let stop = cancel.clone();

        let result = m
            .service()
            .paste_queued(
                &m.request(Paste::Copy, "Documents", &["Trip"], "Backup"),
                &cancel,
                move |report| {
                    if report.files_done >= 2 {
                        stop.cancel();
                    }
                },
            )
            .await;

        assert_eq!(result.err(), Some(Error::Cancelled));
        let copied = m.names("Backup/Trip");
        assert!(copied.len() >= 2 && copied.len() < 5, "{copied:?}");
        assert!(
            copied.iter().all(|name| !name.starts_with('.')),
            "no part file"
        );
        assert_eq!(
            m.names("Documents/Trip").len(),
            5,
            "the originals are whole"
        );
    }

    #[tokio::test]
    async fn a_move_to_another_volume_copies_then_trashes_each_original_once_it_is_whole() {
        let m = mac();
        m.write("Documents/Trip/1.jpg", b"11");
        m.write("Documents/Trip/day2/2.jpg", b"222");
        m.write("Documents/a.txt", b"hello");
        let (log, progress) = reports();

        let summary = m
            .crossing()
            .paste_queued(
                &m.request(Paste::Move, "Documents", &["a.txt", "Trip"], "Backup"),
                &Cancel::new(),
                progress,
            )
            .await
            .unwrap();

        assert_eq!(m.read("Backup/Trip/day2/2.jpg"), b"222");
        assert_eq!(m.read("Backup/a.txt"), b"hello");
        assert!(!m.exists("Documents/a.txt") && !m.exists("Documents/Trip"));
        assert_eq!(m.trash_calls(), 2, "one call for each selected item");
        let removal = summary.removal.unwrap();
        assert!(removal.from_mac);
        assert_eq!(removal.removed, 3, "files in what went to the Trash");
        assert!(removal.left.is_empty() && removal.kept.is_empty());
        let log = log.lock().unwrap();
        let trashing: Vec<_> = log
            .iter()
            .filter(|r| r.trashing)
            .map(|r| (r.files_done, r.files_total))
            .collect();
        assert_eq!(trashing, [(0, 2), (1, 2), (2, 2)]);
        assert!(log.iter().filter(|r| !r.removing).all(|r| r.moving));
    }

    #[tokio::test]
    async fn the_inline_pane_move_to_another_volume_is_a_copy_and_a_trash_too() {
        let m = mac();
        m.write("Documents/a.txt", b"hello");

        let summary = m
            .crossing()
            .paste(&m.request(Paste::Move, "Documents", &["a.txt"], "Backup"))
            .await
            .unwrap();

        assert_eq!(m.read("Backup/a.txt"), b"hello");
        assert!(!m.exists("Documents/a.txt"));
        assert_eq!(m.trash_calls(), 1);
        assert_eq!(summary.done, 1);
    }

    #[tokio::test]
    async fn a_move_to_another_disk_trashes_its_originals_off_the_async_worker() {
        // As the delete's test: each Trash call waits for the other task,
        // which can answer only while the one worker is free.
        let m = mac();
        m.write("Documents/a.txt", b"1");
        m.write("Documents/b.txt", b"2");
        let (started, waiting) = std::sync::mpsc::channel::<()>();
        let (go, heard) = std::sync::mpsc::channel::<()>();
        let (started, heard) = (std::sync::Mutex::new(started), std::sync::Mutex::new(heard));
        let mut service = Service::with_trash(
            m.home.path().to_path_buf(),
            Arc::new(move |path: &Path| {
                started.lock().unwrap().send(()).unwrap();
                heard
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| io::Error::other("the app was held up"))?;
                fs::remove_file(path)
            }),
        );
        service.copy_moves = true;
        let other = tokio::spawn(async move {
            for _ in 0..2 {
                while waiting.try_recv().is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                go.send(()).unwrap();
            }
        });

        let summary = service
            .paste_queued(
                &m.request(Paste::Move, "Documents", &["a.txt", "b.txt"], "Backup"),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        other.await.unwrap();
        assert_eq!(summary.removal.unwrap().left, Vec::<String>::new());
        assert!(!m.exists("Documents/a.txt") && !m.exists("Documents/b.txt"));
        assert_eq!(m.read("Backup/a.txt"), b"1");
    }

    #[tokio::test]
    async fn an_original_stays_when_its_copy_failed_or_the_trash_refuses() {
        let m = mac();
        m.write("Documents/a.txt", b"1");
        m.write("Documents/locked.txt", b"secret");
        m.write("Documents/c.txt", b"3");
        let locked = m.path("Documents/locked.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut service = m.service_refusing(|path| path.ends_with("c.txt"));
        service.copy_moves = true;

        let summary = service
            .paste_queued(
                &m.request(
                    Paste::Move,
                    "Documents",
                    &["a.txt", "locked.txt", "c.txt"],
                    "Backup",
                ),
                &Cancel::new(),
                |_| {},
            )
            .await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        let summary = summary.unwrap();

        assert!(!m.exists("Documents/a.txt"), "copied and trashed");
        assert!(
            m.exists("Documents/locked.txt"),
            "never copied, so it stays"
        );
        assert!(m.exists("Documents/c.txt"), "the Trash said no");
        let removal = summary.removal.unwrap();
        assert_eq!(removal.left, ["locked.txt", "c.txt"]);
        assert_eq!(removal.kept.len(), 1);
        assert_eq!(removal.kept[0].label, "c.txt");
        assert!(matches!(removal.kept[0].why, Why::Refused(_)));
        assert_eq!(summary.failures.len(), 1);
    }

    #[tokio::test]
    async fn an_original_stays_when_its_copy_is_not_whole_by_the_time_it_would_go() {
        let m = mac();
        m.write("Documents/a.txt", b"hello");
        let copy = m.path("Backup/a.txt");

        let summary = m
            .crossing()
            .paste_queued(
                &m.request(Paste::Move, "Documents", &["a.txt"], "Backup"),
                &Cancel::new(),
                move |report| {
                    // The copy is done and the Trash is about to be asked:
                    // something cuts the copy short.
                    if report.trashing && report.files_done == 0 {
                        fs::write(&copy, b"he").unwrap();
                    }
                },
            )
            .await
            .unwrap();

        assert!(m.exists("Documents/a.txt"), "the original stays");
        assert_eq!(m.trash_calls(), 0);
        let removal = summary.removal.unwrap();
        assert_eq!(removal.kept[0].why, Why::Unchecked);
        assert_eq!(removal.left, ["a.txt"]);
    }

    #[tokio::test]
    async fn a_folder_holding_something_that_cannot_be_copied_stays_whole_after_a_move() {
        let m = mac();
        m.write("Documents/Trip/1.jpg", b"11");
        let _socket =
            std::os::unix::net::UnixListener::bind(m.path("Documents/Trip/live.sock")).unwrap();

        let summary = m
            .crossing()
            .paste_queued(
                &m.request(Paste::Move, "Documents", &["Trip"], "Backup"),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(
            m.read("Backup/Trip/1.jpg"),
            b"11",
            "what could be was copied"
        );
        assert!(!m.exists("Backup/Trip/live.sock"));
        assert!(
            m.exists("Documents/Trip/live.sock"),
            "the original is whole"
        );
        assert_eq!(m.trash_calls(), 0);
        let removal = summary.removal.unwrap();
        assert_eq!(removal.kept[0].why, Why::NotCopied);
        assert_eq!(removal.left, ["Trip"]);
    }

    #[tokio::test]
    async fn something_that_is_neither_a_file_nor_a_folder_is_that_items_failure() {
        let m = mac();
        let _socket =
            std::os::unix::net::UnixListener::bind(m.path("Documents/live.sock")).unwrap();
        m.write("Documents/a.txt", b"1");

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["live.sock", "a.txt"], "Backup"))
            .await
            .unwrap();

        assert_eq!(summary.done, 1);
        assert_eq!(summary.failures[0].label, "live.sock");
        assert_eq!(m.names("Backup"), ["a.txt"]);
    }

    #[tokio::test]
    async fn a_tree_that_is_far_too_deep_is_refused_before_anything_is_made() {
        let m = mac();
        let mut deep = m.path("Documents/Deep");
        for _ in 0..70 {
            deep = deep.join("d");
        }
        fs::create_dir_all(&deep).unwrap();

        let result = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["Deep"], "Backup"))
            .await;

        assert_eq!(result.err(), Some(Error::TransferTooLarge));
        assert!(m.names("Backup").is_empty());
    }

    #[tokio::test]
    async fn a_link_back_up_the_tree_is_a_link_and_does_not_loop() {
        let m = mac();
        m.write("Documents/Trip/1.jpg", b"1");
        std::os::unix::fs::symlink("..", m.path("Documents/Trip/up")).unwrap();

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["Trip"], "Backup"))
            .await
            .unwrap();

        assert_eq!(summary.done, 1);
        assert_eq!(
            fs::read_link(m.path("Backup/Trip/up")).unwrap(),
            Path::new("..")
        );
    }

    // ---- the pieces ----

    #[test]
    fn a_name_is_claimed_once_and_numbered_after_that() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join("Trip")).unwrap();
        let mut taken = Taken::of(dir.path()).unwrap();

        assert_eq!(taken.claim("a.txt", false), "a (1).txt");
        assert_eq!(
            taken.claim("A.TXT", false),
            "A (2).TXT",
            "case does not matter"
        );
        assert_eq!(taken.claim("b.txt", false), "b.txt");
        assert_eq!(taken.claim("b.txt", false), "b (1).txt");
        assert_eq!(taken.claim("Trip", true), "Trip (1)");
    }

    #[test]
    fn a_move_leaves_what_is_at_the_new_name_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("from.txt"), b"new").unwrap();
        fs::write(dir.path().join("to.txt"), b"mine").unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::create_dir(dir.path().join("b")).unwrap();

        let file = move_without_replacing(&dir.path().join("from.txt"), &dir.path().join("to.txt"));
        let folder = move_without_replacing(&dir.path().join("a"), &dir.path().join("b"));

        assert_eq!(file.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(folder.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dir.path().join("from.txt")).unwrap(), b"new");
        assert_eq!(fs::read(dir.path().join("to.txt")).unwrap(), b"mine");
        assert!(dir.path().join("a").is_dir());
    }

    #[test]
    fn a_copy_whose_name_is_taken_at_the_last_moment_goes_under_the_next_one_if_it_may() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.txt");
        fs::write(&source, b"new").unwrap();
        fs::write(dir.path().join("a.txt"), b"mine").unwrap();

        let renumbered = copy_file(&source, dir.path(), "a.txt", true).unwrap();
        let refused = copy_file(&source, dir.path(), "a.txt", false);

        assert_eq!(renumbered, "a (1).txt");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"mine");
        assert_eq!(fs::read(dir.path().join("a (1).txt")).unwrap(), b"new");
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        let mut left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["a (1).txt", "a.txt", "src.txt"], "no part file");
    }

    #[test]
    fn a_folder_whose_name_is_taken_at_the_last_moment_goes_under_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("Trip")).unwrap();

        let made = make_folder(dir.path(), "Trip", true).unwrap();

        assert_eq!(made, "Trip (1)");
    }

    // ---- 5,000 files ----

    #[tokio::test]
    async fn a_folder_of_five_thousand_files_is_copied_in_the_pane_without_a_search_for_each() {
        let m = mac();
        for i in 0..5_000 {
            m.write(
                &format!("Documents/Big/{i:05}.jpg"),
                format!("photo {i}").as_bytes(),
            );
        }
        let started = Instant::now();

        let summary = m
            .service()
            .paste(&m.request(Paste::Copy, "Documents", &["Big"], "Backup"))
            .await
            .unwrap();

        eprintln!("5,000 files copied in the pane in {:?}", started.elapsed());
        assert_eq!(summary.done, 1);
        assert_eq!(m.names("Backup/Big").len(), 5_000);
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    #[tokio::test]
    async fn five_thousand_selected_files_are_named_and_moved_in_linear_time() {
        let m = mac();
        for i in 0..5_000 {
            m.write(&format!("Documents/{i:05}.jpg"), b"x");
        }
        let names: Vec<String> = (0..5_000).map(|i| format!("{i:05}.jpg")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let started = Instant::now();

        let summary = m
            .service()
            .paste(&m.request(Paste::Move, "Documents", &names, "Backup"))
            .await
            .unwrap();

        eprintln!("5,000 selected files moved in {:?}", started.elapsed());
        assert_eq!(summary.done, 5_000);
        assert_eq!(m.names("Backup").len(), 5_000);
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    #[tokio::test]
    async fn a_five_thousand_file_folder_moves_in_one_rename() {
        let m = mac();
        for i in 0..5_000 {
            m.write(&format!("Documents/Big/{i:05}.jpg"), b"x");
        }
        let started = Instant::now();

        m.service()
            .paste(&m.request(Paste::Move, "Documents", &["Big"], "Backup"))
            .await
            .unwrap();

        eprintln!("5,000-file folder moved in {:?}", started.elapsed());
        assert_eq!(m.names("Backup/Big").len(), 5_000);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a rename, not a walk"
        );
    }
}
