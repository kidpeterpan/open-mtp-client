//! Checking copies byte for byte: each phone file is read in
//! windows and compared with the Mac file of the same name as it arrives.
//! Nothing is written or changed on either side. Sizes and dates (what a
//! transfer checks) can't catch a copy damaged in place; this can, at the
//! cost of reading the phone's files again, so it runs only when asked.

use std::path::PathBuf;
use std::time::Instant;

use mtp_rs::mtp::{ByteRange, ObjectHandle, ObjectInfo, Storage};
use tokio::io::AsyncReadExt;

use super::mirror::relative;
use super::service::{Failure, Service, WINDOW, check_within_size, phone_answers, size_limit};
use crate::dto::{
    Cancel, CheckRequest, Compared, Direction, Error, FileFailure, FileOutcome, FileReport,
    REPORT_MOST, TransferProgress, TransferSummary,
};
use crate::service::{explorer, local, names};

/// A phone file whose Mac file is there at a size that may match: read.
pub(super) struct ToRead {
    label: String,
    handle: ObjectHandle,
    /// The size the phone gives (MTP's "4 GB or more" is no size).
    size: u64,
    mac: PathBuf,
    mac_len: u64,
}

impl Service {
    /// Compares the selected phone items, everything inside folders too,
    /// with what has the same names in the Mac folder. A file is identical,
    /// differs (its sizes, or at a byte), is on one side only, or couldn't be
    /// read; the report says which, file by file. Sizes are compared before
    /// anything is read, so a file of another size costs the phone nothing.
    pub async fn check_contents(
        &self,
        request: &CheckRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let started = Instant::now();
        let mac_root = PathBuf::from(&request.mac_folder);
        match tokio::fs::metadata(&mac_root).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(Error::PathNotFound(request.mac_folder.clone())),
            Err(err) => return Err(local::mapper::to_error(&err, &request.mac_folder)),
        }

        // Only the selected names are asked about, then each folder is read
        // for real (a file changed in place keeps its folder's item count).
        let folder = self
            .explorer
            .open_folder_confirmed(
                request.storage_id,
                &request.phone_folder,
                &request.names,
                false,
            )
            .await?;
        let partial_reads = folder.session.capabilities().supports_partial_download;
        let mut lines: Vec<FileReport> = Vec::new();
        let mut found: Vec<(String, ObjectInfo)> = Vec::new();
        for name in &request.names {
            let Ok(object) = folder.exact_child(&request.phone_folder, name) else {
                lines.push(line(name.clone(), FileOutcome::NotOnPhone));
                continue;
            };
            if !object.is_folder() {
                found.push((name.clone(), object));
                continue;
            }
            let path = explorer::normalize_path(&format!("{}/{name}", request.phone_folder));
            let mut walk = self
                .explorer
                .walk_from(request.storage_id, &path, true, cancel.clone())
                .await?
                .fresh();
            let before = found.len();
            while let Some(piece) = walk.next_raw().await {
                let (update, objects) = piece?;
                if update.restart {
                    found.truncate(before);
                }
                // What a truncated walk missed would go unchecked unsaid.
                if update.truncated {
                    return Err(Error::TransferTooLarge);
                }
                found.extend(
                    objects
                        .into_iter()
                        .filter(|(_, object)| !object.is_folder())
                        .map(|(folder, object)| {
                            let below = relative(&path, &folder, &object.filename);
                            (format!("{name}/{below}"), object)
                        }),
                );
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
        }

        let mut to_read = Vec::new();
        for (label, object) in found {
            match pair(&mac_root, label, &object).await {
                Ok(item) => to_read.push(item),
                Err(done) => lines.push(*done),
            }
        }

        let mut state = TransferProgress {
            direction: Direction::Download,
            files_total: to_read.len(),
            bytes_total: to_read.iter().map(ToRead::bytes).sum(),
            ..TransferProgress::default()
        };
        progress(state.clone());
        let mut failures = Vec::new();
        for item in &to_read {
            state.current_file = item.label.clone();
            state.file_bytes_done = 0;
            state.file_bytes_total = item.bytes();
            let outcome = self
                .compare_one(
                    &folder.storage,
                    partial_reads,
                    item,
                    cancel,
                    &mut state,
                    started,
                    &progress,
                )
                .await;
            // What was not read of a file that ended early leaves the totals.
            state.bytes_total -= item.bytes().saturating_sub(state.file_bytes_done);
            state.files_done += 1;
            match outcome {
                Ok(outcome) => lines.push(line(item.label.clone(), outcome)),
                Err(failure) if failure.stops_the_batch() => {
                    return Err(self.to_error(failure).await);
                }
                Err(failure) => {
                    let error = failure.into_error();
                    failures.push(FileFailure {
                        label: item.label.clone(),
                        error: error.clone(),
                    });
                    lines.push(line(item.label.clone(), FileOutcome::Failed(error)));
                }
            }
            state.elapsed = started.elapsed();
            progress(state.clone());
        }

        let compared = count(&lines);
        let report_left_out = lines.len().saturating_sub(REPORT_MOST);
        lines.truncate(REPORT_MOST);
        Ok(TransferSummary {
            direction: Direction::Download,
            files: compared.total(),
            bytes: state.bytes_done,
            failures,
            destination: request.mac_folder.clone(),
            first_item: request.names.first().cloned(),
            compared: Some(compared),
            report: lines,
            report_left_out,
            ..TransferSummary::default()
        })
    }

    /// Reads one phone file and compares it with the Mac's as each window
    /// arrives. A phone that reads from an offset stops at the first byte
    /// that differs (not asking for the next window is the safe way to
    /// stop); one that sends whole files only is read to its end, since
    /// breaking off its one long read can wedge it.
    #[allow(clippy::too_many_arguments)]
    async fn compare_one(
        &self,
        storage: &Storage,
        partial_reads: bool,
        item: &ToRead,
        cancel: &Cancel,
        state: &mut TransferProgress,
        started: Instant,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) -> Result<FileOutcome, Failure> {
        let mac_failure = |err| Failure::Mac(err, item.mac.clone());
        let mut mac = tokio::fs::File::open(&item.mac)
            .await
            .map_err(mac_failure)?;
        let limit = size_limit(item.size);
        let heard = self.device.activity();
        let mut buffer = Vec::new();
        let mut read: u64 = 0;
        let mut differs: Option<u64> = None;
        let take = |piece: &[u8], read: &mut u64, state: &mut TransferProgress| {
            let len = piece.len() as u64;
            *read += len;
            state.file_bytes_done += len;
            state.bytes_done += len;
            state.elapsed = started.elapsed();
            heard.heard();
            progress(state.clone());
        };

        if partial_reads {
            let mut download = phone_answers(
                &heard,
                self.stall_limit,
                storage.download_windowed(item.handle, ByteRange::Full, WINDOW),
            )
            .await?
            .map_err(Failure::Device)?;
            while let Some(window) =
                phone_answers(&heard, self.stall_limit, download.next_window()).await?
            {
                let window = window.map_err(Failure::Device)?;
                check_within_size(read, window.len(), limit)?;
                let at = first_difference(&mut mac, &window, &mut buffer)
                    .await
                    .map_err(mac_failure)?;
                differs = at.map(|at| read + at);
                take(&window, &mut read, state);
                if differs.is_some() {
                    break;
                }
                if cancel.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
            }
        } else {
            let mut download = phone_answers(
                &heard,
                self.stall_limit,
                storage.download(item.handle, ByteRange::Full),
            )
            .await?
            .map_err(Failure::Device)?;
            while let Some(chunk) =
                phone_answers(&heard, self.stall_limit, download.next_chunk()).await?
            {
                let chunk = chunk.map_err(Failure::Device)?;
                check_within_size(read, chunk.len(), limit)?;
                // Compared until the first difference, which stands; the rest is
                // only drained.
                if differs.is_none() {
                    let at = first_difference(&mut mac, &chunk, &mut buffer)
                        .await
                        .map_err(mac_failure)?;
                    differs = differs.or(at.map(|at| read + at));
                }
                take(&chunk, &mut read, state);
                if cancel.is_cancelled() {
                    let _ = download.cancel(mtp_rs::mtp::DEFAULT_CANCEL_TIMEOUT).await;
                    return Err(Failure::Cancelled);
                }
            }
        }
        if let Some(at) = differs {
            return Ok(FileOutcome::Differs { at });
        }
        // Everything the phone sent matched: the Mac's may still go on.
        let mut more = [0u8; 1];
        if mac.read(&mut more).await.map_err(mac_failure)? > 0 {
            return Ok(FileOutcome::Differs { at: read });
        }
        Ok(FileOutcome::Identical)
    }
}

impl ToRead {
    /// What reading it costs: the phone's size, or the Mac's when the phone
    /// gives none.
    fn bytes(&self) -> u64 {
        if self.size == u64::from(u32::MAX) {
            self.mac_len
        } else {
            self.size
        }
    }
}

/// The phone file `label` with its Mac file, or what was found without
/// reading it: no such Mac file, sizes that differ, a name the phone
/// shouldn't have given, a Mac file that can't be asked about.
pub(super) async fn pair(
    mac_root: &std::path::Path,
    label: String,
    object: &ObjectInfo,
) -> Result<ToRead, Box<FileReport>> {
    // The phone's names are untrusted: each must be one plain name, or the
    // Mac path could lead out of the folder.
    if !label.split('/').all(names::is_safe_component) {
        return Err(Box::new(line(
            label.clone(),
            FileOutcome::Failed(Error::UnsafeName(label)),
        )));
    }
    let mac = mac_root.join(&label);
    let metadata = match tokio::fs::metadata(&mac).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(Box::new(line(label, FileOutcome::NotOnMac))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Box::new(line(label, FileOutcome::NotOnMac)));
        }
        Err(err) => {
            let error = local::mapper::to_error(&err, &mac.to_string_lossy());
            return Err(Box::new(line(label, FileOutcome::Failed(error))));
        }
    };
    let known = object.size != u64::from(u32::MAX);
    if known && object.size != metadata.len() {
        return Err(Box::new(line(
            label,
            FileOutcome::SizesDiffer {
                phone: object.size,
                mac: metadata.len(),
            },
        )));
    }
    Ok(ToRead {
        label,
        handle: object.handle,
        size: object.size,
        mac,
        mac_len: metadata.len(),
    })
}

/// Reads as many bytes from `mac` as `piece` holds and says where they first
/// differ, relative to the piece: where the Mac file ends, if it ends first.
pub(super) async fn first_difference(
    mac: &mut tokio::fs::File,
    piece: &[u8],
    buffer: &mut Vec<u8>,
) -> std::io::Result<Option<u64>> {
    buffer.resize(piece.len(), 0);
    let mut filled = 0;
    while filled < piece.len() {
        let n = mac.read(&mut buffer[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    let mismatch = piece[..filled]
        .iter()
        .zip(&buffer[..filled])
        .position(|(phone, mac)| phone != mac);
    Ok(match mismatch {
        Some(at) => Some(at as u64),
        None if filled < piece.len() => Some(filled as u64),
        None => None,
    })
}

fn line(path: String, outcome: FileOutcome) -> FileReport {
    FileReport {
        path,
        from: None,
        outcome,
        original: None,
    }
}

/// The report's lines counted.
fn count(lines: &[FileReport]) -> Compared {
    let mut compared = Compared::default();
    for line in lines {
        match line.outcome {
            FileOutcome::Identical => compared.identical += 1,
            FileOutcome::Differs { .. } | FileOutcome::SizesDiffer { .. } => compared.differ += 1,
            FileOutcome::NotOnMac | FileOutcome::NotOnPhone => compared.missing += 1,
            _ => compared.unreadable += 1,
        }
    }
    compared
}
