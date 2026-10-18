//! Get Info's count of a phone folder: everything under it, hidden items
//! too, as Finder counts. It is the search's walk, so it is bounded like
//! every other request, stops between pieces when cancelled, and reuses the
//! folders the app already remembers.

use std::sync::Arc;

use super::Service;
use crate::dto::{Cancel, Entry, Error, Tally};

/// MTP's size for "4 GB or more": not a size.
const UNKNOWN_SIZE: u64 = 0xFFFF_FFFF;

impl Service {
    /// Counts the files, folders and bytes under the folder `path` (not the
    /// folder itself), telling `report` the count so far after each piece.
    pub async fn tally(
        self: &Arc<Self>,
        storage_id: u64,
        path: &str,
        cancel: Cancel,
        report: impl Fn(Tally) + Send + Sync,
    ) -> Result<Tally, Error> {
        let mut walk = self
            .walk_from(storage_id, path, true, cancel.clone())
            .await?;
        let mut tally = Tally::default();
        while let Some(update) = walk.next().await {
            let update = update?;
            // The phone re-keyed its files and the walk began again.
            if update.restart {
                tally = Tally::default();
            }
            for found in &update.found {
                count(&mut tally, &found.entry);
            }
            tally.incomplete |= update.truncated;
            report(tally);
            if update.done {
                return Ok(tally);
            }
        }
        // The walk ends without its last piece only when it was cancelled
        // (an error comes as an error); a part count is never a total.
        Err(Error::Cancelled)
    }
}

fn count(tally: &mut Tally, entry: &Entry) {
    if entry.is_folder {
        tally.folders += 1;
    } else if entry.size_bytes == UNKNOWN_SIZE {
        tally.add_file(&entry.name, None);
    } else {
        tally.add_file(&entry.name, Some(entry.size_bytes));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;
    use crate::service::device;
    use crate::service::testing::virtual_phone;

    struct Phone {
        _root: tempfile::TempDir,
        service: Arc<Service>,
        storage: u64,
    }

    /// A phone with DCIM/Camera/{a.jpg, b.mp4}, DCIM/.hidden.jpg,
    /// Music/song.mp3 and notes.txt.
    async fn phone(extra: &[(&str, usize)]) -> Phone {
        let root = tempfile::tempdir().unwrap();
        let mut files = vec![
            ("DCIM/Camera/a.jpg", 2),
            ("DCIM/Camera/b.mp4", 3),
            ("DCIM/.hidden.jpg", 1),
            ("Music/song.mp3", 5),
            ("notes.txt", 4),
        ];
        files.extend_from_slice(extra);
        for (path, size) in files {
            let at = root.path().join(path);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, vec![7u8; size]).unwrap();
        }
        let device = Arc::new(device::Service::new(device::Source::Virtual(
            virtual_phone(root.path()),
        )));
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        Phone {
            _root: root,
            service: Arc::new(Service::new(device)),
            storage,
        }
    }

    #[tokio::test]
    async fn a_folder_is_counted_with_everything_under_it_hidden_items_too() {
        let p = phone(&[]).await;
        let reports = Mutex::new(Vec::new());

        let tally = p
            .service
            .tally(p.storage, "/DCIM", Cancel::new(), |t| {
                reports.lock().unwrap().push(t)
            })
            .await
            .unwrap();

        assert_eq!(
            tally,
            Tally {
                files: 3,
                folders: 1,
                bytes: 6,
                by_kind: [3, 3, 0, 0, 0, 0, 0],
                ..Tally::default()
            },
            "Camera, a.jpg, b.mp4 and .hidden.jpg; not DCIM itself"
        );
        assert_eq!(reports.lock().unwrap().last(), Some(&tally));
    }

    #[tokio::test]
    async fn the_whole_storage_can_be_counted() {
        let p = phone(&[]).await;

        let tally = p
            .service
            .tally(p.storage, "/", Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!((tally.files, tally.folders, tally.bytes), (5, 3, 15));
    }

    #[tokio::test]
    async fn counting_a_folder_just_listed_reads_nothing_again() {
        let names: Vec<String> = (0..5_000)
            .map(|n| format!("Camera/IMG_{n:05}.jpg"))
            .collect();
        let extra: Vec<(&str, usize)> = names.iter().map(|name| (name.as_str(), 1)).collect();
        let p = phone(&extra).await;
        p.service.list(p.storage, "/Camera", true).await.unwrap();
        let before = p.service.full_reads();

        let tally = p
            .service
            .tally(p.storage, "/Camera", Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!((tally.files, tally.bytes), (5_000, 5_000));
        assert!(
            p.service.full_reads() - before <= 1,
            "{} full reads",
            p.service.full_reads() - before
        );
    }

    #[tokio::test]
    async fn a_cancelled_count_stops_and_says_so() {
        let p = phone(&[]).await;
        let cancel = Cancel::new();
        cancel.cancel();

        let result = p.service.tally(p.storage, "/", cancel, |_| {}).await;

        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    }

    #[tokio::test]
    async fn a_phone_that_is_gone_ends_the_count_with_an_error_not_a_number() {
        let p = phone(&[]).await;
        p.service.device.disconnect().await.unwrap();

        let result = p.service.tally(p.storage, "/", Cancel::new(), |_| {}).await;

        assert!(result.is_err(), "{result:?}");
        assert!(!matches!(result, Err(Error::Cancelled)), "{result:?}");
    }

    #[tokio::test]
    async fn a_folder_that_is_not_there_is_not_found() {
        let p = phone(&[]).await;

        let result = p
            .service
            .tally(p.storage, "/Nowhere", Cancel::new(), |_| {})
            .await;

        assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
    }

    #[test]
    fn a_file_of_4_gb_or_more_is_counted_without_bytes() {
        let mut tally = Tally::default();
        let entry = |size_bytes| Entry {
            id: 1,
            name: "x".into(),
            is_folder: false,
            size_bytes,
            modified: None,
        };

        count(&mut tally, &entry(UNKNOWN_SIZE));
        count(&mut tally, &entry(10));

        assert_eq!((tally.files, tally.bytes, tally.unknown_sizes), (2, 10, 1));
        let mut both = tally;
        both.add(&tally);
        assert_eq!((both.files, both.bytes, both.unknown_sizes), (4, 20, 2));
    }
}
