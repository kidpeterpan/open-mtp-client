//! Importing a phone's new photos and videos into a Mac folder.
//!
//! The storage's DCIM folder is walked (hidden folders such as `.thumbnails`
//! and Android's `.trashed-…` never count, and the walk reuses the explorer's
//! index, so a preview followed by the import, or an import done again, asks
//! each unchanged folder only how many items it holds). Every photo and video
//! goes straight into the destination, or into a folder for its day
//! (`2026-10-17`, the date the pane shows) when asked; DCIM's own folders
//! (Camera, Screenshots, 100ANDRO) are not kept, and a name that is taken
//! gets ` (1)` as Finder's "keep both" would.
//!
//! A file is new unless it is in the destination already (same size and date,
//! under its name or a numbered one: `resolve_file`) or it is in the record of
//! what was imported from this phone before. The record is what lets a photo
//! the user has since moved, deleted or filed elsewhere stay on the phone, and
//! what makes switching between dated and flat safe. Only copies checked to be
//! whole go in the record.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use mtp_rs::StorageId;
use mtp_rs::mtp::{ObjectHandle, ObjectInfo, Storage};

use super::relay::Nowhere;
use super::service::{
    How, Origin, Outcome, Plan, Removed, Run, Service, Step, note_fate, resolve_file,
};
use crate::dto::{
    Cancel, Direction, Error, FileKind, ImportPreview, ImportRequest, Kept, Original, Removal,
    TransferProgress, TransferSummary, Why,
};
use crate::service::failures::whole_batch_failure;
use crate::service::{explorer, local};

/// Where a phone keeps what its camera takes.
pub(super) const DCIM: &str = "/DCIM";

/// What an import brings over: photos and videos.
fn is_media(name: &str) -> bool {
    matches!(FileKind::of(name), Some(FileKind::Photo | FileKind::Video))
}

/// The record of what was imported from each phone, one file per phone in a
/// folder of the app's own (never anything the user sees).
#[derive(Debug, Clone)]
pub struct ImportRecord {
    dir: PathBuf,
}

impl ImportRecord {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The phone's file: its serial number spelled in hex, so any serial makes
    /// one plain file name.
    fn file(&self, serial: &str) -> PathBuf {
        let hex: String = serial.bytes().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{hex}.tsv"))
    }

    /// Each line of the phone's record by its key (`record_key`: path, size
    /// and date), with where the copy went when the record says (a fourth
    /// field; lines written before it have none).
    async fn lines(&self, serial: &str) -> HashMap<String, Option<PathBuf>> {
        tokio::fs::read_to_string(self.file(serial))
            .await
            .map(|text| text.lines().filter_map(parse_line).collect())
            .unwrap_or_default()
    }

    /// What was imported from the phone `serial`; nothing if there is no
    /// record yet, or it can't be read.
    pub(super) async fn load(&self, serial: &str) -> HashSet<String> {
        self.lines(serial).await.into_keys().collect()
    }

    /// Where each import from the phone `serial` put its copy, by key, for
    /// the imports that wrote it down.
    pub(super) async fn targets(&self, serial: &str) -> HashMap<String, PathBuf> {
        self.lines(serial)
            .await
            .into_iter()
            .filter_map(|(key, target)| Some((key, target?)))
            .collect()
    }

    /// Adds `imported` (each key with where its copy went) to the phone's
    /// record, written whole to a temporary file and renamed over the old
    /// one, so a crash leaves one or the other.
    pub(super) async fn add(
        &self,
        serial: &str,
        imported: impl IntoIterator<Item = (String, PathBuf)>,
    ) -> std::io::Result<()> {
        let mut all = self.lines(serial).await;
        let mut changed = false;
        for (key, target) in imported {
            // The key holds the phone's name, which is untrusted: one with a
            // tab or a line break in it would read back as other fields or
            // other lines (a forged target, another photo's key), so it is
            // not written at all. Such a photo is offered again next time,
            // which costs a question, never a wrong answer.
            if key.matches('\t').count() != 2 || key.contains(['\n', '\r']) {
                continue;
            }
            // A path that can't be written on one line is left out; the key
            // alone still keeps the photo from coming back.
            let target = target
                .to_str()
                .filter(|path| !path.contains(['\t', '\n', '\r']))
                .map(PathBuf::from);
            if all.get(&key) != Some(&target) {
                all.insert(key, target);
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let mut lines: Vec<String> = all
            .into_iter()
            .map(|(key, target)| match target {
                Some(target) => format!("{key}\t{}", target.display()),
                None => key,
            })
            .collect();
        lines.sort();
        tokio::fs::create_dir_all(&self.dir).await?;
        let file = self.file(serial);
        let part = file.with_extension("tsv.part");
        tokio::fs::write(&part, lines.join("\n") + "\n").await?;
        tokio::fs::rename(&part, &file).await
    }
}

/// One record line: its key (the first three fields) and the copy's path
/// (a fourth, if there is one).
fn parse_line(line: &str) -> Option<(String, Option<PathBuf>)> {
    let mut fields = line.splitn(4, '\t');
    let (path, size, date) = (fields.next()?, fields.next()?, fields.next()?);
    let target = fields
        .next()
        .filter(|target| target.starts_with('/'))
        .map(PathBuf::from);
    Some((format!("{path}\t{size}\t{date}"), target))
}

/// How the record knows a file: where it is on the phone, its size and its
/// date, so a different photo saved later under the same name is new.
pub(super) fn record_key(folder: &str, object: &ObjectInfo) -> String {
    let date = object
        .modified
        .map(|d| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                d.year, d.month, d.day, d.hour, d.minute, d.second
            )
        })
        .unwrap_or_default();
    let path = if folder.ends_with('/') {
        format!("{folder}{}", object.filename)
    } else {
        format!("{folder}/{}", object.filename)
    };
    format!("{path}\t{}\t{date}", object.size)
}

/// The folder a file goes into, by the date the pane shows: "2026-10-17",
/// "2026-10" or "2026/10"; `None` for straight into the destination, or a
/// file without a date.
fn date_folder(object: &ObjectInfo, folders: crate::dto::ImportFolders) -> Option<PathBuf> {
    let day = explorer::shown_date(object)?;
    folder_for(day.year, day.month, day.day, folders)
}

/// The folder for a date in each layout (pure).
fn folder_for(
    year: u16,
    month: u8,
    day: u8,
    folders: crate::dto::ImportFolders,
) -> Option<PathBuf> {
    use crate::dto::ImportFolders;
    match folders {
        ImportFolders::None => None,
        ImportFolders::Day => Some(PathBuf::from(format!("{year:04}-{month:02}-{day:02}"))),
        ImportFolders::Month => Some(PathBuf::from(format!("{year:04}-{month:02}"))),
        ImportFolders::YearMonth => {
            Some(PathBuf::from(format!("{year:04}")).join(format!("{month:02}")))
        }
    }
}

/// An import worked out: the steps, the record key of each file step, and
/// what is needed to carry them out.
struct Planned {
    plan: Plan,
    keys: Vec<Option<String>>,
    storage: Storage,
    partial_reads: bool,
    serial: String,
    /// Left out because the record has them.
    imported_before: usize,
    /// For each step, the phone file it copies and the folder it is in (none
    /// for the Mac's day folders): what a freeing import may remove.
    originals: Vec<Option<(String, Origin)>>,
}

impl Service {
    /// Puts what is imported in `record`, and leaves out what it already has.
    pub fn with_imports(mut self, record: ImportRecord) -> Self {
        self.imports = Some(record);
        self
    }

    /// What `import` would do now: how many photos and videos are new, and
    /// how big. `seen` is told how many photos and videos the walk has found
    /// so far.
    pub async fn import_preview(
        &self,
        request: &ImportRequest,
        cancel: &Cancel,
        seen: impl Fn(usize) + Send + Sync,
    ) -> Result<ImportPreview, Error> {
        let planned = self.plan_import(request, cancel, &seen).await?;
        let mut preview = ImportPreview {
            already: planned.imported_before,
            ..ImportPreview::default()
        };
        for step in &planned.plan.steps {
            match step {
                Step::File {
                    how: How::AlreadyThere,
                    ..
                } => preview.already += 1,
                Step::File { .. } => {
                    preview.new_files += 1;
                    preview.new_bytes += step.bytes_to_copy();
                }
                Step::Folder(_) => {}
            }
        }
        Ok(preview)
    }

    /// Copies the phone's new photos and videos into the destination (see the
    /// module docs). Like a download it can be cancelled, carries on from part
    /// files and keeps going past a file that fails; the summary's
    /// `already_there` counts what was not new.
    pub async fn import(
        &self,
        request: &ImportRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let started = Instant::now();
        let planned = self.plan_import(request, cancel, &|_| {}).await?;
        let destination = PathBuf::from(&request.destination);
        let executed = self
            .run_steps::<Nowhere>(
                &planned.plan,
                Run {
                    storage: &planned.storage,
                    partial_reads: planned.partial_reads,
                    destination: &destination,
                    // Originals that are about to go are forced to the disk
                    // first, as a move's are.
                    moving: request.remove_from_phone,
                    started,
                    cancel,
                    onward: None,
                },
                &progress,
            )
            .await?;

        // A phone without a serial number can't be told from the next one.
        if let Some(record) = self.imports.as_ref().filter(|_| !planned.serial.is_empty()) {
            let whole = planned
                .keys
                .iter()
                .zip(&planned.plan.steps)
                .zip(&executed.outcomes)
                .filter(|(_, outcome)| matches!(outcome, Outcome::Verified))
                .filter_map(|((key, step), _)| match (key, step) {
                    (Some(key), Step::File { target, .. }) => Some((key.clone(), target.clone())),
                    _ => None,
                });
            // Not being able to write it down costs a question next time, not
            // this import.
            let _ = record.add(&planned.serial, whole).await;
        }

        let copied = executed.state.files_done - executed.already_there - executed.not_copied;
        if let Some(error) = whole_batch_failure(copied, executed.already_there, &executed.failures)
        {
            return Err(error);
        }
        let mut fates = vec![None; planned.plan.steps.len()];
        let removal = if request.remove_from_phone {
            Some(
                self.remove_imported(
                    &planned,
                    request.storage_id,
                    &executed.outcomes,
                    cancel,
                    &progress,
                    &mut fates,
                )
                .await,
            )
        } else {
            None
        };
        let (report, report_left_out) = super::service::report(
            &planned.plan,
            &executed.outcomes,
            &executed.failures,
            request.remove_from_phone.then_some(fates.as_slice()),
        );
        Ok(TransferSummary {
            removal,
            direction: Direction::Download,
            files: copied,
            bytes: executed.state.bytes_done,
            renamed: planned.plan.renamed,
            already_there: executed.already_there + planned.imported_before,
            resumed: executed.resumed,
            failures: executed.failures,
            destination: request.destination.clone(),
            first_item: planned.plan.first_item.clone(),
            arrival: Some(super::service::arrival(&planned.plan, &executed.outcomes)),
            report,
            report_left_out,
            ..TransferSummary::default()
        })
    }

    async fn plan_import(
        &self,
        request: &ImportRequest,
        cancel: &Cancel,
        seen: &(impl Fn(usize) + Sync),
    ) -> Result<Planned, Error> {
        let destination = PathBuf::from(&request.destination);
        match tokio::fs::metadata(&destination).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(Error::PathNotFound(request.destination.clone())),
            Err(err) => return Err(local::mapper::to_error(&err, &request.destination)),
        }

        // Hidden folders are never imported, whatever the panes show.
        let mut walk = self
            .explorer
            .walk_from(request.storage_id, DCIM, false, cancel.clone())
            .await?;
        let mut media: Vec<(String, ObjectInfo)> = Vec::new();
        while let Some(piece) = walk.next_raw().await {
            let (update, found) = piece?;
            if update.restart {
                media.clear();
            }
            media.extend(
                found
                    .into_iter()
                    .filter(|(_, object)| !object.is_folder() && is_media(&object.filename)),
            );
            seen(media.len());
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        media.sort_by(|a, b| (&a.0, &a.1.filename).cmp(&(&b.0, &b.1.filename)));

        let (session, epoch) = self.device.session_at().await?;
        let storage = match self
            .limits
            .ask(session.storage(StorageId(request.storage_id)))
            .await
        {
            Ok(storage) => storage,
            Err(err) => return Err(self.device.handle_error_at(epoch, err).await),
        };
        let partial_reads = session.capabilities().supports_partial_download;
        let serial = session.device_info().serial_number.clone();
        let record = match &self.imports {
            Some(record) if !serial.is_empty() => record.load(&serial).await,
            _ => HashSet::new(),
        };

        let mut steps = Vec::new();
        let mut keys = Vec::new();
        let mut originals = Vec::new();
        let mut days: HashSet<PathBuf> = HashSet::new();
        let mut claimed: HashSet<String> = HashSet::new();
        let (mut imported_before, mut renamed) = (0, 0);
        let mut first_item = None;
        for (folder, object) in &media {
            let key = record_key(folder, object);
            if record.contains(&key) {
                imported_before += 1;
                continue;
            }
            let dir = match date_folder(object, request.folders) {
                Some(folder) => destination.join(folder),
                None => destination.clone(),
            };
            if dir != destination && days.insert(dir.clone()) {
                steps.push(Step::Folder(dir.clone()));
                keys.push(None);
                originals.push(None);
            }
            let (target, how) = resolve_file(
                &dir.join(&object.filename),
                object.size,
                object.modified,
                partial_reads,
                &claimed,
            )
            .await;
            claimed.insert(target.to_string_lossy().to_lowercase());
            let label = relative(&target, &destination);
            if how != How::AlreadyThere {
                renamed += usize::from(target.file_name() != Some(object.filename.as_ref()));
                first_item.get_or_insert_with(|| label.clone());
            }
            steps.push(Step::File {
                handle: object.handle,
                target,
                label,
                size: object.size,
                modified: object.modified,
                how,
            });
            keys.push(Some(key));
            originals.push(Some((
                folder.clone(),
                Origin::file(
                    object.handle,
                    object.filename.clone(),
                    object.size,
                    object.modified,
                ),
            )));
        }
        Ok(Planned {
            plan: Plan {
                steps,
                // Nothing is removed from the phone after an import.
                origins: Vec::new(),
                items: Vec::new(),
                unwalked: false,
                renamed,
                first_item,
                replaces: Vec::new(),
                skipped: Vec::new(),
            },
            keys,
            storage,
            partial_reads,
            serial,
            imported_before,
            originals,
        })
    }

    /// After an import that frees the phone: removes, one file at a time,
    /// each original whose copy was checked, looked up again first (still the
    /// file that was copied, unchanged), never a folder. A file whose copy
    /// couldn't be checked stays, and says so; a cancel or a phone that stops
    /// answering leaves the rest. The pane's memory is told what went.
    async fn remove_imported(
        &self,
        planned: &Planned,
        storage_id: u64,
        outcomes: &[Outcome],
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
        fates: &mut [Option<Original>],
    ) -> Removal {
        let mut removal = Removal::default();
        let owed = planned
            .originals
            .iter()
            .zip(outcomes)
            .filter(|(original, outcome)| original.is_some() && **outcome == Outcome::Verified)
            .count();
        let started = Instant::now();
        let mut state = TransferProgress {
            direction: Direction::Download,
            moving: true,
            removing: true,
            files_total: owed,
            ..TransferProgress::default()
        };
        progress(state.clone());
        let mut gone: BTreeMap<String, Vec<ObjectHandle>> = BTreeMap::new();
        for (index, (original, outcome)) in planned.originals.iter().zip(outcomes).enumerate() {
            let Some((folder, origin)) = original else {
                continue;
            };
            let label = format!("{}/{}", folder.trim_start_matches('/'), origin.name());
            match outcome {
                Outcome::Verified => {}
                Outcome::Unchecked => {
                    note_fate(fates, index, Original::Kept(Why::Unchecked));
                    removal.kept.push(Kept {
                        label,
                        why: Why::Unchecked,
                    });
                    continue;
                }
                // Not copied: its failure says why, and it stays.
                Outcome::NotCopied => continue,
            }
            if removal.stopped.is_none() && cancel.is_cancelled() {
                removal.stopped = Some(Error::Cancelled);
            }
            if removal.stopped.is_some() {
                continue;
            }
            match self.remove_one(&planned.storage, origin).await {
                Removed::Done => {
                    note_fate(fates, index, Original::Removed);
                    removal.removed += 1;
                    removal.freed += origin.size();
                    gone.entry(folder.clone())
                        .or_default()
                        .push(origin.handle());
                    state.files_done += 1;
                    state.elapsed = started.elapsed();
                    progress(state.clone());
                }
                Removed::Kept(why) => {
                    note_fate(fates, index, Original::Kept(why.clone()));
                    removal.kept.push(Kept { label, why });
                }
                Removed::Stopped(failure) => removal.stopped = Some(self.to_error(failure).await),
            }
        }
        self.explorer.note_removed(storage_id, &gone, &[]).await;
        removal
    }
}

fn relative(target: &Path, destination: &Path) -> String {
    target
        .strip_prefix(destination)
        .unwrap_or(target)
        .to_string_lossy()
        .into_owned()
}
