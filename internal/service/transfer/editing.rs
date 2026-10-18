//! Editing a phone file on the Mac: the file is fetched, as
//! Quick Look's are (one plain name, a size the phone gives, nothing over
//! `PREVIEW_LARGEST`), into a working copy of its own under the app's
//! support folder (not a cache, which macOS may empty while an edit is
//! unsent), with a manifest of where it came from, and left writable. Sending
//! it asks the phone about that one file first: still the file that was
//! fetched, it is replaced; changed since, the edit goes in beside it under a
//! numbered name, so neither side's change is lost.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use mtp_rs::mtp::ObjectInfo;
use serde::{Deserialize, Serialize};

use super::service::{Service, size_is_known};
use crate::dto::{
    Cancel, Conflict, CopyState, DownloadRequest, EditRequest, Error, MOST_EDITS, PREVIEW_LARGEST,
    Paste, TransferProgress, TransferSummary, UploadRequest, WorkingCopy,
};
use crate::service::{local, names};

const MANIFEST: &str = "manifest.json";

/// The manifest as it is kept (serde lives here, not in the dto).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    version: u32,
    phone: String,
    storage: u64,
    folder: String,
    name: String,
    fetched_size: u64,
    fetched_ms: u64,
    phone_size: u64,
    phone_date: String,
}

/// The phone's date for a file as one string, to see later whether the file
/// is still the one fetched.
fn phone_date(object: &ObjectInfo) -> String {
    object
        .modified
        .map(|d| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                d.year, d.month, d.day, d.hour, d.minute, d.second
            )
        })
        .unwrap_or_default()
}

/// One short, plain folder name for a phone file.
fn copy_id(request: &EditRequest) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        &request.phone,
        request.storage_id,
        &request.folder,
        &request.name,
    )
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// What a copy on the Mac is now; `None` when it is not a file there.
pub fn copy_state(path: &Path) -> Option<CopyState> {
    let found = std::fs::symlink_metadata(path).ok()?;
    if !found.is_file() {
        return None;
    }
    let modified_ms = found
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(CopyState {
        size: found.len(),
        modified_ms: u64::try_from(modified_ms).unwrap_or(u64::MAX),
    })
}

impl Service {
    /// Keeps working copies for editing under `root` (made private), and
    /// lets editing happen.
    pub fn with_editing(mut self, root: PathBuf) -> Self {
        self.editing = Some(root);
        self
    }

    /// Fetches the phone file into a working copy and says where it is. A
    /// file already being edited is not fetched again: its copy, edits and
    /// all, is the answer.
    pub async fn edit_on_mac(&self, request: &EditRequest) -> Result<WorkingCopy, Error> {
        let Some(root) = &self.editing else {
            return Err(Error::Unsupported);
        };
        let copies = self.working_copies().await;
        let id = copy_id(request);
        if let Some(open) = copies.iter().find(|copy| copy.id == id) {
            return Ok(open.clone());
        }
        if copies.len() >= MOST_EDITS {
            return Err(Error::TooManyEdits);
        }
        let folder = self
            .explorer
            .open_folder_confirmed(
                request.storage_id,
                &request.folder,
                std::slice::from_ref(&request.name),
                true,
            )
            .await?;
        let object = folder.exact_child(&request.folder, &request.name)?;
        // The phone's name is untrusted and becomes a Mac path.
        if !names::is_safe_component(&object.filename)
            || object.is_folder()
            || !size_is_known(object.size)
            || object.size > PREVIEW_LARGEST
        {
            return Err(Error::NotEditable);
        }

        let mac = |err: std::io::Error, path: &Path| {
            local::mapper::to_error(&err, &path.to_string_lossy())
        };
        let place = root.join(&id);
        for dir in [root.as_path(), place.as_path()] {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|err| mac(err, dir))?;
            tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|err| mac(err, dir))?;
        }
        let summary = self
            .download_as(
                Paste::Copy,
                &DownloadRequest {
                    storage_id: request.storage_id,
                    source: request.folder.clone(),
                    names: vec![request.name.clone()],
                    destination: place.to_string_lossy().into_owned(),
                },
                &Cancel::new(),
                |_| {},
            )
            .await;
        let path = place.join(&object.filename);
        let fetched = match (summary, copy_state(&path)) {
            (Ok(summary), Some(state)) if summary.failures.is_empty() => state,
            (Err(error), _) => {
                let _ = tokio::fs::remove_dir_all(&place).await;
                return Err(error);
            }
            _ => {
                let _ = tokio::fs::remove_dir_all(&place).await;
                return Err(Error::NotEditable);
            }
        };
        let copy = WorkingCopy {
            id,
            phone: request.phone.clone(),
            storage_id: request.storage_id,
            folder: request.folder.clone(),
            name: object.filename.clone(),
            path: path.to_string_lossy().into_owned(),
            fetched,
            phone_size: object.size,
            phone_date: phone_date(&object),
        };
        if let Err(err) = write_manifest(&place, &copy).await {
            let _ = tokio::fs::remove_dir_all(&place).await;
            return Err(mac(err, &place));
        }
        Ok(copy)
    }

    /// The working copies there are, from their manifests (so they come back
    /// after a relaunch); a folder whose manifest is missing or odd, or
    /// whose copy has gone, is left out.
    pub async fn working_copies(&self) -> Vec<WorkingCopy> {
        let Some(root) = &self.editing else {
            return Vec::new();
        };
        let mut copies = Vec::new();
        let Ok(mut folders) = tokio::fs::read_dir(root).await else {
            return copies;
        };
        while let Ok(Some(entry)) = folders.next_entry().await {
            let place = entry.path();
            let Ok(text) = tokio::fs::read_to_string(place.join(MANIFEST)).await else {
                continue;
            };
            let Ok(file) = serde_json::from_str::<ManifestFile>(&text) else {
                continue;
            };
            // A hand-edited manifest names nothing outside its own folder.
            if !names::is_safe_component(&file.name) || !file.folder.starts_with('/') {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            let path = place.join(&file.name);
            if copy_state(&path).is_none() {
                continue;
            }
            copies.push(WorkingCopy {
                id,
                phone: file.phone,
                storage_id: file.storage,
                folder: file.folder,
                name: file.name,
                path: path.to_string_lossy().into_owned(),
                fetched: CopyState {
                    size: file.fetched_size,
                    modified_ms: file.fetched_ms,
                },
                phone_size: file.phone_size,
                phone_date: file.phone_date,
            });
        }
        copies.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        copies.truncate(MOST_EDITS);
        copies
    }

    /// Sends the edited copy back: the phone's file replaced if it is still
    /// the one fetched, else the edit put beside it (numbered). The copy goes
    /// once the phone has it.
    pub async fn send_edit(
        &self,
        copy: &WorkingCopy,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let place = self.copy_place(copy)?;
        // Only to the phone it came from: another phone's folder of the same
        // path is not where this edit belongs, whatever queued it.
        if self.device.open_key().await.as_deref() != Some(copy.phone.as_str()) {
            return Err(Error::OtherPhone);
        }
        let folder = self
            .explorer
            .open_folder_confirmed(
                copy.storage_id,
                &copy.folder,
                std::slice::from_ref(&copy.name),
                false,
            )
            .await?;
        let unchanged = folder
            .exact_child(&copy.folder, &copy.name)
            .is_ok_and(|object| {
                !object.is_folder()
                    && object.size == copy.phone_size
                    && phone_date(&object) == copy.phone_date
            });
        let conflict = if unchanged {
            Conflict::Replace
        } else {
            Conflict::KeepBoth
        };
        let summary = self
            .upload(
                &UploadRequest {
                    sources: vec![copy.path.clone()],
                    storage_id: copy.storage_id,
                    destination: copy.folder.clone(),
                },
                conflict,
                cancel,
                progress,
            )
            .await?;
        if summary.failures.is_empty() {
            let _ = tokio::fs::remove_dir_all(&place).await;
        }
        Ok(summary)
    }

    /// Forgets a working copy and its edits.
    pub async fn discard_edit(&self, copy: &WorkingCopy) -> Result<(), Error> {
        let place = self.copy_place(copy)?;
        tokio::fs::remove_dir_all(&place)
            .await
            .map_err(|err| local::mapper::to_error(&err, &place.to_string_lossy()))
    }

    /// The copy's own folder, only if it is one of the app's.
    fn copy_place(&self, copy: &WorkingCopy) -> Result<PathBuf, Error> {
        let root = self.editing.as_ref().ok_or(Error::Unsupported)?;
        if !names::is_safe_component(&copy.id) {
            return Err(Error::NotEditable);
        }
        let place = root.join(&copy.id);
        if Path::new(&copy.path).parent() != Some(place.as_path()) {
            return Err(Error::NotEditable);
        }
        Ok(place)
    }
}

/// Writes the manifest whole to a temporary file and renames it into place.
async fn write_manifest(place: &Path, copy: &WorkingCopy) -> std::io::Result<()> {
    let file = ManifestFile {
        version: 1,
        phone: copy.phone.clone(),
        storage: copy.storage_id,
        folder: copy.folder.clone(),
        name: copy.name.clone(),
        fetched_size: copy.fetched.size,
        fetched_ms: copy.fetched.modified_ms,
        phone_size: copy.phone_size,
        phone_date: copy.phone_date.clone(),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    let part = place.join(".manifest.json.part");
    tokio::fs::write(&part, json).await?;
    tokio::fs::rename(&part, place.join(MANIFEST)).await
}
