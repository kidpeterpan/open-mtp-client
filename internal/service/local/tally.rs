//! Get Info's count of a Mac folder: every file and folder under it, hidden
//! ones too, as Finder counts. Links are counted as what they are and never
//! followed (a link to a folder above would loop for ever), and a folder
//! macOS keeps private is counted as unreadable rather than ending the count.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::Service;
use super::mapper;
use crate::dto::{Cancel, Error, Tally};

/// How often the count so far is told, at most.
const REPORT_EVERY: Duration = Duration::from_millis(100);

impl Service {
    /// Counts the files, folders and bytes under the folder `path` (not the
    /// folder itself) off the async threads, telling `report` the count so far
    /// now and then. Cancelling stops it between entries.
    pub async fn tally(
        &self,
        path: &str,
        cancel: Cancel,
        report: impl Fn(Tally) + Send + 'static,
    ) -> Result<Tally, Error> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || count(&path, &cancel, &report))
            .await
            // The count itself broke, which nobody asked for: say it failed.
            .unwrap_or_else(|_| Err(Error::General("The count could not finish.".to_owned())))
    }
}

fn count(path: &str, cancel: &Cancel, report: &dyn Fn(Tally)) -> Result<Tally, Error> {
    let root = PathBuf::from(path);
    let mut tally = Tally::default();
    let mut waiting = vec![root.clone()];
    let mut told = Instant::now();
    while let Some(folder) = waiting.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if folder == root => return Err(mapper::to_error(&err, path)),
            Err(_) => {
                tally.unreadable += 1;
                continue;
            }
        };
        for entry in entries.flatten() {
            // The entry's own kind: a link is a link, whatever it points at.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                tally.folders += 1;
                waiting.push(entry.path());
            } else {
                let size = entry.metadata().map_or(0, |metadata| metadata.len());
                tally.add_file(&entry.file_name().to_string_lossy(), Some(size));
            }
            if told.elapsed() >= REPORT_EVERY {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                report(tally);
                told = Instant::now();
            }
        }
    }
    report(tally);
    Ok(tally)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A folder with a.txt (3 bytes), .hidden (1), Sub/b.txt (5) and
    /// Sub/Deeper/, and a link inside Sub pointing back at the top.
    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path();
        fs::write(at.join("a.txt"), b"abc").unwrap();
        fs::write(at.join(".hidden"), b"h").unwrap();
        fs::create_dir_all(at.join("Sub/Deeper")).unwrap();
        fs::write(at.join("Sub/b.txt"), b"bbbbb").unwrap();
        std::os::unix::fs::symlink(at, at.join("Sub/loop")).unwrap();
        dir
    }

    fn service() -> Service {
        Service::new(PathBuf::from("/nowhere"))
    }

    #[tokio::test]
    async fn a_folder_is_counted_hidden_files_too_and_a_link_is_never_followed() {
        let dir = folder();
        let told = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&told);

        let tally = service()
            .tally(dir.path().to_str().unwrap(), Cancel::new(), move |t| {
                log.lock().unwrap().push(t)
            })
            .await
            .unwrap();

        assert_eq!(
            (tally.files, tally.folders),
            (4, 2),
            "a.txt, .hidden, b.txt, the link; Sub, Deeper"
        );
        assert_eq!(
            tally.bytes,
            9 + fs::symlink_metadata(dir.path().join("Sub/loop"))
                .unwrap()
                .len(),
            "the link's own size, not what it points at"
        );
        assert_eq!(told.lock().unwrap().last(), Some(&tally));
    }

    #[tokio::test]
    async fn a_folder_that_cannot_be_read_is_counted_as_such_and_the_rest_goes_on() {
        let dir = folder();
        let private = dir.path().join("Private");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("secret.txt"), b"s").unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();

        let tally = service()
            .tally(dir.path().to_str().unwrap(), Cancel::new(), |_| {})
            .await;
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();

        let tally = tally.unwrap();
        assert_eq!(tally.unreadable, 1);
        assert_eq!(
            (tally.files, tally.folders),
            (4, 3),
            "Private itself is counted"
        );
    }

    #[tokio::test]
    async fn a_cancelled_count_stops_and_a_missing_folder_is_not_found() {
        let dir = folder();
        let cancel = Cancel::new();
        cancel.cancel();

        let cancelled = service()
            .tally(dir.path().to_str().unwrap(), cancel, |_| {})
            .await;
        let missing = service()
            .tally(
                dir.path().join("Gone").to_str().unwrap(),
                Cancel::new(),
                |_| {},
            )
            .await;

        assert!(matches!(cancelled, Err(Error::Cancelled)), "{cancelled:?}");
        assert!(
            matches!(missing, Err(Error::PathNotFound(_))),
            "{missing:?}"
        );
    }
}
