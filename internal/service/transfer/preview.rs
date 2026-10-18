//! Fetching phone files to the Mac to look at them (Quick Look, or opening
//! them in their app).
//!
//! The files go into a cache of the app's own under the temporary folder,
//! one folder per phone (by serial number: two phones can share storage ids)
//! and storage, and one per phone folder below it, private to the user
//! (`0700`). They are fetched as any download is (names checked, part files,
//! the phone's date kept), so a second look at the same file finds it there
//! with the same size and date and copies nothing. A copy that differs from
//! the phone's file is removed first, so the look is always at the phone's
//! file under its own name. The cache is kept under `PREVIEW_CACHE`: before a
//! fetch, the folders used longest ago go first.
//!
//! Every copy is marked as come from elsewhere (`com.apple.quarantine`, as a
//! browser marks a download), so opening one goes through the Mac's checks
//! (a phone can hold a `.command` or an installer), and made read-only, so an
//! app that opens it says it is locked: edits to it would never reach the
//! phone.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::mapper;
use super::service::{Service, size_is_known};
use crate::dto::{
    Cancel, DownloadRequest, Error, Fetched, PREVIEW_CACHE, PREVIEW_LARGEST, PREVIEW_MOST, Paste,
    PreviewRequest, TransferProgress,
};
use crate::service::{local, names};

/// Touched on every fetch into a folder of the cache: when it was last used.
const USED: &str = ".used";

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

/// A phone folder as one short, plain name: a deep path spelled out could be
/// longer than a Mac file name may be.
fn folder_key(path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

impl Service {
    /// Keeps the copies fetched for a look under `root` (made private), and
    /// lets such fetches happen.
    pub fn with_preview_cache(mut self, root: PathBuf) -> Self {
        self.preview_cache = Some(root);
        self
    }

    /// Fetches `request`'s files into the cache (see the module docs): at
    /// most `PREVIEW_MOST`, no folders, and nothing over `PREVIEW_LARGEST` or
    /// of a size the phone doesn't give.
    pub async fn fetch_for_preview(
        &self,
        request: &PreviewRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<Fetched, Error> {
        let Some(root) = &self.preview_cache else {
            return Err(Error::Unsupported);
        };
        let mut fetched = Fetched {
            left_out: request.names.len().saturating_sub(PREVIEW_MOST),
            ..Fetched::default()
        };
        let asked: Vec<String> = request.names.iter().take(PREVIEW_MOST).cloned().collect();
        let folder = self
            .explorer
            .open_folder_confirmed(request.storage_id, &request.folder, &asked, true)
            .await?;
        let serial = folder.key.0.clone();
        let mut wanted: Vec<String> = Vec::new();
        let mut objects = Vec::new();
        for name in &asked {
            let object = folder.exact_child(&request.folder, name)?;
            // The phone's name is untrusted, and it is joined to a path on the
            // Mac (and a stale copy of that name removed) before the download
            // would check it: anything but one plain name is refused here.
            if !names::is_safe_component(&object.filename) {
                fetched.failed.push(name.clone());
            // The Mac ignores case: of two names that differ only in it, the
            // second would land beside the first under another name.
            } else if differs_only_in_case(&wanted, name) {
                fetched.failed.push(name.clone());
            } else if object.is_folder() {
                fetched.folders.push(name.clone());
            } else if !size_is_known(object.size) || object.size > PREVIEW_LARGEST {
                fetched.too_big.push(name.clone());
            } else {
                wanted.push(name.clone());
                objects.push(object);
            }
        }
        if wanted.is_empty() {
            return Ok(fetched);
        }

        let mac = |err: std::io::Error, path: &Path| {
            local::mapper::to_error(&err, &path.to_string_lossy())
        };
        tokio::fs::create_dir_all(root)
            .await
            .map_err(|err| mac(err, root))?;
        tokio::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|err| mac(err, root))?;
        let place = root
            .join(format!("{}-{}", hex(&serial), request.storage_id))
            .join(folder_key(&request.folder));
        let needed: u64 = objects.iter().map(|object| object.size).sum();
        prune(root, &place, PREVIEW_CACHE.saturating_sub(needed)).await;
        tokio::fs::create_dir_all(&place)
            .await
            .map_err(|err| mac(err, &place))?;
        let _ = tokio::fs::write(place.join(USED), b"").await;

        // A copy that is not the phone's file as it is now goes, so the new one
        // takes its name rather than a numbered one beside it.
        for object in &objects {
            let copy = place.join(&object.filename);
            if copy.parent() != Some(place.as_path()) {
                continue;
            }
            let same = tokio::fs::symlink_metadata(&copy).await.is_ok_and(|found| {
                found.is_file()
                    && found.len() == object.size
                    && object
                        .modified
                        .and_then(mapper::to_system_time)
                        .zip(found.modified().ok())
                        .is_some_and(|(phone, mac)| mapper::same_moment(phone, mac))
            });
            if !same {
                let _ = tokio::fs::remove_file(&copy).await;
            }
        }

        let summary = self
            .download_as(
                Paste::Copy,
                &DownloadRequest {
                    storage_id: request.storage_id,
                    source: request.folder.clone(),
                    names: wanted.clone(),
                    destination: place.to_string_lossy().into_owned(),
                },
                cancel,
                progress,
            )
            .await?;
        for name in wanted {
            let path = place.join(&name);
            if summary.failures.iter().any(|failure| failure.label == name)
                || mark_as_from_elsewhere(&path).await.is_err()
            {
                fetched.failed.push(name);
            } else {
                fetched.paths.push(path.to_string_lossy().into_owned());
            }
        }
        Ok(fetched)
    }
}

/// Marks a fetched copy as come from elsewhere and makes it read-only. (A
/// copy already marked and locked by an earlier look is unlocked to be
/// marked again: the mark needs write access.)
async fn mark_as_from_elsewhere(path: &Path) -> std::io::Result<()> {
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).await?;
    let marked = crate::service::quarantine::mark(path);
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).await?;
    marked
}

/// Whether `name` is one of `taken` but for case. (Android's storage tells
/// `a.jpg` and `A.jpg` apart; the Mac's doesn't.)
fn differs_only_in_case(taken: &[String], name: &str) -> bool {
    let folded = name.to_lowercase();
    taken.iter().any(|other| other.to_lowercase() == folded)
}

/// Removes whole folders of the cache, used longest ago first, until what is
/// left takes up at most `room` bytes; `keep` stays whatever it holds.
async fn prune(root: &Path, keep: &Path, room: u64) {
    let mut folders: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
    let Ok(mut phones) = tokio::fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(phone)) = phones.next_entry().await {
        let Ok(mut inside) = tokio::fs::read_dir(phone.path()).await else {
            continue;
        };
        while let Ok(Some(folder)) = inside.next_entry().await {
            let path = folder.path();
            let used = tokio::fs::metadata(path.join(USED))
                .await
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            folders.push((used, path.clone(), size_of(&path).await));
        }
    }
    let mut total: u64 = folders.iter().map(|(_, _, size)| size).sum();
    folders.sort_by_key(|(used, _, _)| *used);
    for (_, path, size) in folders {
        if total <= room {
            break;
        }
        if path == keep {
            continue;
        }
        if tokio::fs::remove_dir_all(&path).await.is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// The bytes of the files directly in `folder` (the cache keeps no deeper).
async fn size_of(folder: &Path) -> u64 {
    let mut total = 0;
    if let Ok(mut entries) = tokio::fs::read_dir(folder).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(metadata) = entry.metadata().await
                && metadata.is_file()
            {
                total += metadata.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_name_is_ever_joined_to_the_cache() {
        // What `fetch_for_preview` checks before it touches the Mac: a phone
        // can report any name, the virtual one only those its folder holds.
        for hostile in ["../../.zshrc", "..", ".", "a/b", "/etc/passwd", ""] {
            assert!(!names::is_safe_component(hostile), "{hostile:?}");
        }
        assert!(names::is_safe_component("IMG_0001.jpg"));
    }

    #[test]
    fn of_two_names_differing_only_in_case_the_second_is_left_on_the_phone() {
        let taken = vec!["a.jpg".to_owned(), "b.jpg".to_owned()];

        assert!(differs_only_in_case(&taken, "A.JPG"));
        assert!(differs_only_in_case(&taken, "a.jpg"));
        assert!(!differs_only_in_case(&taken, "c.jpg"));
        assert_eq!(folder_key("/DCIM").len(), 16);
        assert_ne!(folder_key("/DCIM"), folder_key("/DCIM/Camera"));
    }
}
