use std::sync::Arc;
use std::time::{Duration, Instant};

use mtp_rs::mtp::{ListingItem, ObjectInfo, ObjectListing, Storage};

use super::mapper;
use super::service::{Failure, Service};
use crate::dto::{Cancel, Entry, Error, ListingUpdate};
use crate::service::clock;

/// An update goes out after this many items or this long, whichever is first:
/// often enough that a slow phone shows rows within a blink, rarely enough that
/// a fast one doesn't make the pane redraw per file.
const BATCH_ITEMS: usize = 500;
const BATCH_TIME: Duration = Duration::from_millis(120);

/// A folder being read from the phone piece by piece. Each [`Listing::next`]
/// fetches more items; between fetches nothing is in flight, so dropping the
/// listing or cancelling it never leaves the phone in the middle of a request.
pub struct Listing {
    service: Arc<Service>,
    storage: Storage,
    serial: String,
    storage_id: u64,
    path: String,
    show_hidden: bool,
    cancel: Cancel,
    objects: ObjectListing,
    /// The connection this is being read on; see `Memory`.
    epoch: u64,
    /// The folder as remembered from an earlier full reading, handed over in
    /// one piece instead of reading it again.
    remembered: Option<Vec<ObjectInfo>>,
    /// Everything read so far, to be remembered once the folder is complete.
    read: Vec<ObjectInfo>,
    total: usize,
    loaded: usize,
    /// Items whose details were read, as opposed to handles the phone refused.
    readable: usize,
    first_skipped: Option<mtp_rs::Error>,
    /// Android re-keys files when its media scanner runs, which makes the
    /// handles collected at the start stale; the listing starts over once.
    restarted: bool,
    finished: bool,
}

impl Listing {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        service: Arc<Service>,
        storage: Storage,
        serial: String,
        storage_id: u64,
        path: String,
        show_hidden: bool,
        cancel: Cancel,
        objects: ObjectListing,
        epoch: u64,
        remembered: Option<Vec<ObjectInfo>>,
    ) -> Self {
        let total = objects.total();
        Self {
            service,
            storage,
            serial,
            storage_id,
            path,
            show_hidden,
            cancel,
            objects,
            epoch,
            remembered,
            read: Vec::new(),
            total,
            loaded: 0,
            readable: 0,
            first_skipped: None,
            restarted: false,
            finished: false,
        }
    }

    /// How many items the phone says the folder holds.
    pub fn total(&self) -> usize {
        self.total
    }

    /// The next piece, or `None` once the listing is over (it ended, failed,
    /// or was cancelled). The last piece before the end has `done` set.
    pub async fn next(&mut self) -> Option<Result<ListingUpdate, Error>> {
        if self.finished {
            return None;
        }
        if let Some(children) = self.remembered.take() {
            // The phone reports as many items as when this folder was last
            // read in full, so that reading is shown instead of asking about
            // every file again.
            self.finished = true;
            let entries = children
                .iter()
                .map(|info| mapper::to_entry(info, clock::local_offset_minutes))
                .filter(|entry| self.show_hidden || !entry.is_hidden())
                .collect();
            return Some(Ok(ListingUpdate {
                entries,
                restart: false,
                loaded: self.total,
                total: self.total,
                done: true,
            }));
        }
        let started = Instant::now();
        let mut entries: Vec<Entry> = Vec::new();
        let mut fetched: Vec<mtp_rs::mtp::ObjectInfo> = Vec::new();
        let mut done = false;

        loop {
            if self.cancel.is_cancelled() {
                self.finished = true;
                return None;
            }
            match self.service.limits.next(&mut self.objects).await {
                Some(Ok(ListingItem::Object(info))) => {
                    self.loaded += 1;
                    self.readable += 1;
                    let entry = mapper::to_entry(&info, clock::local_offset_minutes);
                    if self.show_hidden || !entry.is_hidden() {
                        entries.push(entry);
                    }
                    fetched.push(info);
                    if fetched.len() >= BATCH_ITEMS || started.elapsed() >= BATCH_TIME {
                        break;
                    }
                }
                Some(Ok(ListingItem::Skipped(skipped))) => {
                    self.loaded += 1;
                    self.first_skipped.get_or_insert(skipped.error);
                }
                Some(Err(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound))
                    if !self.restarted =>
                {
                    return Some(self.restart().await);
                }
                Some(Err(err)) => {
                    self.finished = true;
                    return Some(Err(self.service.handle_error_at(self.epoch, err).await));
                }
                None => {
                    self.finished = true;
                    done = true;
                    break;
                }
            }
        }

        // A phone that answered for none of its handles didn't show an empty
        // folder, it failed; saying "empty" would read as "everything was
        // deleted".
        if done
            && self.readable == 0
            && let Some(err) = self.first_skipped.take()
        {
            return Some(Err(self.service.handle_error_at(self.epoch, err).await));
        }

        self.service
            .remember(&self.serial, self.storage_id, &self.path, &fetched)
            .await;
        self.read.extend(fetched);
        if done {
            // The whole folder has been read: remember it, so editing it or
            // coming back to it needn't read every file again.
            let key = (self.serial.clone(), self.storage_id, self.path.clone());
            let read = std::mem::take(&mut self.read);
            self.service
                .keep_listing(key, self.epoch, self.total, read)
                .await;
        }
        Some(Ok(ListingUpdate {
            entries,
            restart: false,
            loaded: self.loaded,
            total: self.total,
            done,
        }))
    }

    /// Reads the folder again from its path, after the phone re-keyed items.
    async fn restart(&mut self) -> Result<ListingUpdate, Error> {
        self.restarted = true;
        self.service.forget_folders().await;
        match self
            .service
            .start_listing(&self.storage, &self.serial, self.storage_id, &self.path)
            .await
        {
            Ok((_folder, objects)) => {
                self.total = objects.total();
                self.objects = objects;
                self.read.clear();
                self.loaded = 0;
                self.readable = 0;
                self.first_skipped = None;
                Ok(ListingUpdate {
                    restart: true,
                    total: self.total,
                    ..ListingUpdate::default()
                })
            }
            Err(Failure::PathNotFound) => {
                self.finished = true;
                Err(Error::PathNotFound(self.path.clone()))
            }
            Err(Failure::Client(err)) => {
                self.finished = true;
                Err(self.service.handle_error_at(self.epoch, err).await)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::service::device;
    use crate::service::testing::virtual_phone;

    struct Phone {
        _dir: tempfile::TempDir,
        device: Arc<device::Service>,
        service: Arc<Service>,
        storage: u64,
        serial: String,
    }

    /// A virtual phone whose storage `setup` fills in.
    async fn phone(setup: impl FnOnce(&Path)) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        setup(dir.path());
        let device = Arc::new(device::Service::new(device::Source::Virtual(
            virtual_phone(dir.path()),
        )));
        let serial = device.connect().await.unwrap().serial_number;
        let storage = device.storages().await.unwrap()[0].id;
        Phone {
            _dir: dir,
            device: Arc::clone(&device),
            service: Arc::new(Service::new(device)),
            storage,
            serial,
        }
    }

    /// `/DCIM/Camera` holding `count` photos.
    fn camera(count: usize) -> impl FnOnce(&Path) {
        move |root| {
            fs::create_dir_all(root.join("DCIM/Camera")).unwrap();
            for i in 0..count {
                fs::write(root.join(format!("DCIM/Camera/IMG_{i:05}.jpg")), b"x").unwrap();
            }
        }
    }

    async fn open(phone: &Phone, path: &str, show_hidden: bool) -> Listing {
        open_with(phone, path, show_hidden, Cancel::new())
            .await
            .unwrap()
    }

    async fn open_with(
        phone: &Phone,
        path: &str,
        show_hidden: bool,
        cancel: Cancel,
    ) -> Result<Listing, Error> {
        phone
            .service
            .open_listing(phone.storage, path, show_hidden, cancel, false)
            .await
    }

    async fn updates(mut listing: Listing) -> Vec<Result<ListingUpdate, Error>> {
        let mut all = Vec::new();
        while let Some(update) = listing.next().await {
            all.push(update);
        }
        all
    }

    fn names(updates: &[ListingUpdate]) -> Vec<String> {
        updates
            .iter()
            .flat_map(|update| update.entries.iter().map(|entry| entry.name.clone()))
            .collect()
    }

    fn ok(updates: Vec<Result<ListingUpdate, Error>>) -> Vec<ListingUpdate> {
        updates.into_iter().map(|update| update.unwrap()).collect()
    }

    #[tokio::test]
    async fn a_small_folder_arrives_in_one_finished_piece() {
        let phone = phone(camera(3)).await;

        let all = ok(updates(open(&phone, "/DCIM/Camera", false).await).await);

        assert_eq!(all.len(), 1);
        assert!(all[0].done);
        assert_eq!((all[0].loaded, all[0].total), (3, 3));
        assert_eq!(
            names(&all),
            ["IMG_00000.jpg", "IMG_00001.jpg", "IMG_00002.jpg"]
        );
    }

    #[tokio::test]
    async fn a_big_folder_arrives_in_pieces_that_add_up_to_all_of_it() {
        let phone = phone(camera(1_300)).await;
        let listing = open(&phone, "/DCIM/Camera", false).await;
        assert_eq!(listing.total(), 1_300);

        let all = ok(updates(listing).await);

        assert!(all.len() >= 3, "{} pieces", all.len());
        assert!(all.iter().all(|u| u.entries.len() <= BATCH_ITEMS));
        assert!(all[..all.len() - 1].iter().all(|u| !u.done));
        assert!(all.last().unwrap().done);
        let loaded: Vec<usize> = all.iter().map(|u| u.loaded).collect();
        assert!(
            loaded.windows(2).all(|pair| pair[0] < pair[1]),
            "{loaded:?}"
        );
        assert_eq!(*loaded.last().unwrap(), 1_300);
        assert!(all.iter().all(|u| u.total == 1_300));
        let mut seen = names(&all);
        assert_eq!(seen.len(), 1_300);
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 1_300, "no file twice");
    }

    #[tokio::test]
    async fn hidden_files_are_left_out_but_still_counted_as_loaded() {
        let phone = phone(|root| {
            fs::write(root.join("a.txt"), b"a").unwrap();
            fs::write(root.join(".hidden"), b"h").unwrap();
        })
        .await;

        let shown = ok(updates(open(&phone, "/", false).await).await);
        let all = ok(updates(open(&phone, "/", true).await).await);

        assert_eq!(names(&shown), ["a.txt"]);
        assert_eq!((shown[0].loaded, shown[0].total), (2, 2));
        let mut everything = names(&all);
        everything.sort();
        assert_eq!(everything, [".hidden", "a.txt"]);
    }

    #[tokio::test]
    async fn an_empty_folder_is_one_finished_empty_piece() {
        let phone = phone(|root| fs::create_dir(root.join("Music")).unwrap()).await;

        let all = ok(updates(open(&phone, "/Music", false).await).await);

        assert_eq!(all.len(), 1);
        assert!(all[0].done && all[0].entries.is_empty());
        assert_eq!((all[0].loaded, all[0].total), (0, 0));
    }

    #[tokio::test]
    async fn nothing_comes_after_the_last_piece() {
        let phone = phone(camera(2)).await;
        let mut listing = open(&phone, "/DCIM/Camera", false).await;

        assert!(listing.next().await.unwrap().unwrap().done);

        assert!(listing.next().await.is_none());
        assert!(listing.next().await.is_none());
    }

    #[tokio::test]
    async fn cancelling_before_the_first_piece_ends_it_quietly() {
        let phone = phone(camera(50)).await;
        let cancel = Cancel::new();
        let mut listing = open_with(&phone, "/DCIM/Camera", false, cancel.clone())
            .await
            .unwrap();

        cancel.cancel();

        assert!(listing.next().await.is_none(), "no piece, no error");
    }

    #[tokio::test]
    async fn cancelling_between_pieces_stops_the_rest() {
        let phone = phone(camera(1_300)).await;
        let cancel = Cancel::new();
        let mut listing = open_with(&phone, "/DCIM/Camera", false, cancel.clone())
            .await
            .unwrap();
        let first = listing.next().await.unwrap().unwrap();
        assert!(!first.done);

        cancel.cancel();

        assert!(listing.next().await.is_none());
        assert!(listing.next().await.is_none());
    }

    #[tokio::test]
    async fn a_missing_folder_is_reported_when_opening() {
        let phone = phone(camera(1)).await;

        let result = open_with(&phone, "/DCIM/Nope", false, Cancel::new()).await;

        assert!(matches!(result, Err(Error::PathNotFound(path)) if path == "/DCIM/Nope"));
    }

    #[tokio::test]
    async fn without_a_connected_phone_opening_fails() {
        let phone = phone(camera(1)).await;
        phone.device.disconnect().await.unwrap();

        let result = open_with(&phone, "/", false, Cancel::new()).await;

        assert!(matches!(result, Err(Error::NotConnected)));
    }

    #[tokio::test]
    async fn a_folder_re_keyed_before_opening_is_re_resolved_once() {
        let phone = phone(camera(4)).await;
        // Cache the handle, then let the phone re-key the folder.
        ok(updates(open(&phone, "/DCIM/Camera", false).await).await);
        assert!(mtp_rs::rekey_virtual_object(&phone.serial, Path::new("DCIM/Camera")).is_some());

        let all = ok(updates(open(&phone, "/DCIM/Camera", false).await).await);

        assert_eq!(names(&all).len(), 4);
    }

    #[tokio::test]
    async fn an_empty_folder_re_keyed_before_opening_is_re_resolved() {
        let phone = phone(|root| fs::create_dir(root.join("Music")).unwrap()).await;
        ok(updates(open(&phone, "/Music", false).await).await);
        assert!(mtp_rs::rekey_virtual_object(&phone.serial, Path::new("Music")).is_some());

        let all = ok(updates(open(&phone, "/Music", false).await).await);

        assert_eq!(all.len(), 1);
        assert!(all[0].done);
    }

    #[tokio::test]
    async fn files_re_keyed_while_listing_make_it_start_over_once() {
        let phone = phone(camera(1_200)).await;
        let mut listing = open(&phone, "/DCIM/Camera", false).await;
        let first = listing.next().await.unwrap().unwrap();
        assert!(!first.done && !first.entries.is_empty());

        // What Android's media scanner does mid-listing: every file gets a new
        // handle, so the handles collected at the start no longer exist.
        for i in 0..1_200 {
            let name = format!("DCIM/Camera/IMG_{i:05}.jpg");
            mtp_rs::rekey_virtual_object(&phone.serial, Path::new(&name));
        }
        let restart = listing.next().await.unwrap().unwrap();
        assert!(restart.restart, "told to forget what came before");
        assert!(restart.entries.is_empty() && !restart.done);
        assert_eq!((restart.loaded, restart.total), (0, 1_200));

        let rest = ok(updates(listing).await);
        assert!(rest.last().unwrap().done);
        assert!(rest.iter().all(|u| !u.restart));
        assert_eq!(
            names(&rest).len(),
            1_200,
            "the whole folder, from the start"
        );
    }

    #[tokio::test]
    async fn a_second_re_key_is_an_error_not_an_endless_restart() {
        let phone = phone(camera(1_200)).await;
        let mut listing = open(&phone, "/DCIM/Camera", false).await;
        listing.next().await.unwrap().unwrap();
        let rekey_all = || {
            for i in 0..1_200 {
                let name = format!("DCIM/Camera/IMG_{i:05}.jpg");
                mtp_rs::rekey_virtual_object(&phone.serial, Path::new(&name));
            }
        };
        rekey_all();
        assert!(listing.next().await.unwrap().unwrap().restart);
        listing.next().await.unwrap().unwrap();
        rekey_all();

        let rest = updates(listing).await;

        assert!(rest.last().unwrap().is_err());
    }

    /// The handles of everything in `/DCIM/Camera`, by name.
    async fn handles(phone: &Phone) -> Vec<(String, mtp_rs::mtp::ObjectHandle)> {
        let session = phone.device.session().await.unwrap();
        let storage = session
            .storage(mtp_rs::StorageId(phone.storage))
            .await
            .unwrap();
        let root = storage.list_objects(None).await.unwrap();
        let dcim = root.iter().find(|o| o.filename == "DCIM").unwrap().handle;
        let kids = storage.list_objects(Some(dcim)).await.unwrap();
        let camera = kids.iter().find(|o| o.filename == "Camera").unwrap().handle;
        storage
            .list_objects(Some(camera))
            .await
            .unwrap()
            .into_iter()
            .map(|o| (o.filename, o.handle))
            .collect()
    }

    #[tokio::test]
    async fn one_file_the_phone_will_not_describe_does_not_hide_the_others() {
        let phone = phone(camera(5)).await;
        let handles = handles(&phone).await;
        let bad = handles
            .iter()
            .find(|(n, _)| n == "IMG_00002.jpg")
            .unwrap()
            .1;
        assert!(mtp_rs::force_object_info_error(&phone.serial, bad, None));

        let all = ok(updates(open(&phone, "/DCIM/Camera", false).await).await);

        assert_eq!(names(&all).len(), 4);
        assert!(!names(&all).contains(&"IMG_00002.jpg".to_owned()));
        assert!(all.last().unwrap().done);
        assert_eq!(
            all.last().unwrap().loaded,
            5,
            "the refused one was still passed"
        );
    }

    #[tokio::test]
    async fn a_phone_that_describes_nothing_is_a_failure_not_an_empty_folder() {
        let phone = phone(camera(3)).await;
        for (_, handle) in handles(&phone).await {
            assert!(mtp_rs::force_object_info_error(&phone.serial, handle, None));
        }

        let all = updates(open(&phone, "/DCIM/Camera", false).await).await;

        assert!(all.last().unwrap().is_err(), "{all:?}");
        assert!(
            all.iter()
                .all(|u| u.as_ref().map_or(true, |u| u.entries.is_empty()))
        );
    }

    #[tokio::test]
    async fn listing_remembers_subfolders_so_going_into_one_is_quick() {
        let phone = phone(camera(2)).await;

        ok(updates(open(&phone, "/", false).await).await);

        let key = (phone.serial.clone(), phone.storage, "/DCIM".to_owned());
        assert!(phone.service.folders.lock().await.contains_key(&key));
    }

    #[tokio::test]
    async fn the_pieces_match_what_a_plain_listing_gives() {
        let phone = phone(|root| {
            fs::create_dir_all(root.join("Folder")).unwrap();
            fs::write(root.join("b.txt"), b"bb").unwrap();
            fs::write(root.join("a.txt"), b"a").unwrap();
        })
        .await;

        let mut streamed = ok(updates(open(&phone, "/", false).await).await)
            .into_iter()
            .flat_map(|u| u.entries)
            .collect::<Vec<_>>();
        let mut plain = phone.service.list(phone.storage, "/", false).await.unwrap();
        streamed.sort_by(|a, b| a.name.cmp(&b.name));
        plain.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(streamed, plain);
    }
}
