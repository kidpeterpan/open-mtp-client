//! Copying and moving items inside the phone, with MTP's own `CopyObject` and
//! `MoveObject`, so nothing crosses USB.
//!
//! Phones differ in what they do when the target already has an item of that
//! name, and some do the worst thing (the virtual phone copies a file onto
//! itself and can empty it). So a clash is never left to the phone: a move
//! first renames the item to a free name, and a copy goes through a hidden
//! scratch folder, is renamed there, and is moved into place. Nothing is ever
//! asked of the phone that could overwrite.

use mtp_rs::StorageId;
use mtp_rs::mtp::{Capabilities, ObjectHandle};

use super::bounded::Limits;
use super::service::{Service, join, normalize};
use crate::dto::{Error, FileFailure, Paste, PasteRequest, PasteSummary};
use crate::service::failures::{is_about_one_object, whole_batch_failure};
use crate::service::{mapper, names};

/// The name of the hidden folder a clashing copy is made in.
const SCRATCH: &str = ".open-mtp-client-paste";

/// The phone operations pasting uses; a seam so the order of them, and what
/// happens when one fails half way, can be tested without a phone.
pub(super) trait Pasting: Sync {
    fn copy(
        &self,
        handle: ObjectHandle,
        parent: ObjectHandle,
        storage: Option<StorageId>,
    ) -> impl Future<Output = Result<ObjectHandle, mtp_rs::Error>> + Send;
    fn relocate(
        &self,
        handle: ObjectHandle,
        parent: ObjectHandle,
        storage: Option<StorageId>,
    ) -> impl Future<Output = Result<(), mtp_rs::Error>> + Send;
    fn rename(
        &self,
        handle: ObjectHandle,
        name: &str,
    ) -> impl Future<Output = Result<(), mtp_rs::Error>> + Send;
    fn make_folder(
        &self,
        parent: Option<ObjectHandle>,
        name: &str,
    ) -> impl Future<Output = Result<ObjectHandle, mtp_rs::Error>> + Send;
    /// Deletes a folder with everything in it, each request under `limits`.
    fn remove_folder(
        &self,
        limits: &Limits,
        handle: ObjectHandle,
    ) -> impl Future<Output = Result<(), mtp_rs::Error>> + Send;
}

/// Why one item could not be done.
#[derive(Debug)]
pub(super) enum Refused {
    /// The phone said no, or went quiet.
    Phone(mtp_rs::Error),
    /// The name is taken in the target and this phone cannot rename or move
    /// the way a safe copy under a new name needs.
    NameTaken,
}

impl From<mtp_rs::Error> for Refused {
    fn from(err: mtp_rs::Error) -> Self {
        Refused::Phone(err)
    }
}

/// Whether a phone that advertises `capabilities` can do `mode` at all.
pub(super) fn can_do(capabilities: &Capabilities, mode: Paste) -> bool {
    match mode {
        Paste::Copy => capabilities.can_copy,
        Paste::Move => capabilities.can_move,
    }
}

/// Whether a phone can put an item whose name is taken in the target under a
/// free name without ever asking it to overwrite: a copy is made in a scratch
/// folder, renamed, and moved out; a move renames the item first.
pub(super) fn can_clear_a_clash(capabilities: &Capabilities, mode: Paste) -> bool {
    match mode {
        Paste::Copy => {
            capabilities.can_rename && capabilities.can_move && capabilities.can_create_folder
        }
        Paste::Move => capabilities.can_rename,
    }
}

/// Whether any of `names` meets a name already taken in the target, counting
/// each one as taken for the ones after it (two names that differ only by case
/// are one name to the phone).
pub(super) fn batch_clashes(names: &[&str], taken: &[String]) -> bool {
    let mut taken = taken.to_vec();
    for name in names {
        if taken.iter().any(|other| names::same(other, name)) {
            return true;
        }
        taken.push((*name).to_owned());
    }
    false
}

/// Whether the phone can do this paste by itself: its own copy or move at all,
/// and, when something clashes with a name in the target, the longer way that
/// takes. A phone that can't is handed over to the Mac before anything is
/// touched.
pub(super) fn can_paste_here(capabilities: &Capabilities, mode: Paste, clashes: bool) -> bool {
    can_do(capabilities, mode) && (!clashes || can_clear_a_clash(capabilities, mode))
}

/// A scratch folder name that nothing in the target has.
fn scratch_name(taken: &[String]) -> String {
    (1..)
        .map(|n| format!("{SCRATCH}-{n}"))
        .find(|candidate| !taken.iter().any(|name| names::same(name, candidate)))
        .unwrap_or_else(|| SCRATCH.to_owned())
}

/// An item to copy or move.
pub(super) struct Item<'a> {
    pub handle: ObjectHandle,
    pub name: &'a str,
    pub is_folder: bool,
}

/// Where the items are going.
pub(super) struct Target<'a> {
    pub mode: Paste,
    pub capabilities: &'a Capabilities,
    /// The folder they go into; `None` is the root of the storage.
    pub folder: Option<ObjectHandle>,
    /// Set when the target is on another storage than the items.
    pub storage: Option<StorageId>,
    /// Names already in the target, including the items placed so far.
    pub taken: &'a [String],
    /// Names in the folder the items come from.
    pub source_names: &'a [String],
    /// How long the phone gets to answer each request. A copy or a move to
    /// another storage is one request that works for as long as the item is
    /// big and says nothing meanwhile, so it gets the long limit.
    pub limits: Limits,
}

/// Copies or moves one item, returning its handle in the target and the name
/// it has there. See the module docs for how a name clash is avoided.
pub(super) async fn place_one(
    source: &impl Pasting,
    dest: &impl Pasting,
    target: &Target<'_>,
    item: &Item<'_>,
) -> Result<(ObjectHandle, String), Refused> {
    let parent = target.folder.unwrap_or(ObjectHandle::ROOT);
    let clash = target.taken.iter().any(|name| names::same(name, item.name));
    let caps = target.capabilities;
    let limits = &target.limits;

    match (target.mode, clash) {
        (Paste::Copy, false) => {
            let copy = limits
                .long(source.copy(item.handle, parent, target.storage))
                .await?;
            Ok((copy, item.name.to_owned()))
        }
        (Paste::Copy, true) => {
            // The copy is renamed in the target, so only the target's names matter.
            let free = names::free_name(target.taken, item.name, item.is_folder);
            if !can_clear_a_clash(caps, Paste::Copy) {
                return Err(Refused::NameTaken);
            }
            let scratch = limits
                .ask(dest.make_folder(target.folder, &scratch_name(target.taken)))
                .await?;
            let made = async {
                let copy = limits
                    .long(source.copy(item.handle, scratch, target.storage))
                    .await?;
                limits
                    .scaled(item.is_folder, dest.rename(copy, &free))
                    .await?;
                limits
                    .scaled(item.is_folder, dest.relocate(copy, parent, None))
                    .await?;
                Ok::<_, mtp_rs::Error>(copy)
            }
            .await;
            // The scratch folder never outlives the copy, and takes any
            // half-made copy with it. (A phone that has gone silent is not
            // waited on again for the tidying up.)
            let _ = limits.long(dest.remove_folder(limits, scratch)).await;
            Ok((made?, free))
        }
        (Paste::Move, false) => {
            moved(limits, source, item, parent, target.storage).await?;
            Ok((item.handle, item.name.to_owned()))
        }
        (Paste::Move, true) => {
            if !can_clear_a_clash(caps, Paste::Move) {
                return Err(Refused::NameTaken);
            }
            // Renamed where it is, first, so the name must be free in both places.
            let mut used = target.taken.to_vec();
            used.extend(target.source_names.iter().cloned());
            let free = names::free_name(&used, item.name, item.is_folder);
            limits
                .scaled(item.is_folder, source.rename(item.handle, &free))
                .await?;
            if let Err(err) = moved(limits, source, item, parent, target.storage).await {
                // When the phone answered no, nothing moved and the old name
                // is free again, so put it back. After a connection-level
                // error (a timeout, a reset) the move may have happened with
                // its reply lost, and the item may already be in the target,
                // where its old name belongs to another file: renaming it
                // there would be the very clash this code never leaves to
                // the phone. It keeps its new name.
                if is_about_one_object(&err) {
                    let _ = limits
                        .scaled(item.is_folder, source.rename(item.handle, item.name))
                        .await;
                }
                return Err(err.into());
            }
            Ok((item.handle, free))
        }
    }
}

/// One move request. Within a storage a file is only a change of parent,
/// answered at once; a folder has the phone rewrite the path of everything in
/// it, and a move to another storage has it copy the item and delete the
/// original, before it answers.
async fn moved(
    limits: &Limits,
    phone: &impl Pasting,
    item: &Item<'_>,
    parent: ObjectHandle,
    storage: Option<StorageId>,
) -> Result<(), mtp_rs::Error> {
    let request = phone.relocate(item.handle, parent, storage);
    // A folder's path changes for everything inside it, wherever it goes.
    limits
        .scaled(storage.is_some() || item.is_folder, request)
        .await
}

/// A paste that has been checked and has something to do.
struct Checked {
    from: String,
    source: super::service::Folder,
    items: Vec<mtp_rs::mtp::ObjectInfo>,
    dest: super::service::Folder,
}

impl Service {
    /// Checks a paste the way the phone's own copy and move do, before anything
    /// is touched, and says whether there is anything to do: `None` for no
    /// items, or a move into the folder the items are already in. Refuses a
    /// folder going into itself or below itself, an item that is not there, and
    /// a read-only target. The same rules hold when the copy has to go through
    /// the Mac (`check_paste`).
    async fn checked(&self, request: &PasteRequest) -> Result<Option<Checked>, Error> {
        let from = normalize(&request.from);
        let to = normalize(&request.to);
        let same_folder = request.from_storage == request.to_storage && from == to;
        if request.names.is_empty() || (request.mode == Paste::Move && same_folder) {
            // Nothing to do: no items, or already where it is going.
            return Ok(None);
        }

        let source = self
            .open_folder_confirmed(request.from_storage, &from, &request.names, true)
            .await?;
        let items = request
            .names
            .iter()
            .map(|name| source.exact_child(&from, name))
            .collect::<Result<Vec<_>, _>>()?;
        for item in items.iter().filter(|item| item.is_folder()) {
            let inside = join(&from, &item.filename);
            if request.from_storage == request.to_storage
                && (to == inside || to.starts_with(&format!("{inside}/")))
            {
                return Err(Error::IntoItself(item.filename.clone()));
            }
        }
        let dest = self.open_folder(request.to_storage, &to).await?;
        dest.ensure_writable()?;
        Ok(Some(Checked {
            from,
            source,
            items,
            dest,
        }))
    }

    /// Whether a paste has anything to do, after the checks every paste makes
    /// (see `checked`). For the copy that goes through the Mac when the phone
    /// cannot copy or move by itself.
    pub(crate) async fn check_paste(&self, request: &PasteRequest) -> Result<bool, Error> {
        Ok(self.checked(request).await?.is_some())
    }

    /// Copies or moves the items inside the phone. A name that is already
    /// taken in the target is never overwritten: the item gets `name (1)`,
    /// `name (2)`… (the same rule as downloads). Folders cannot go into
    /// themselves. An item the phone refuses is listed in the summary and the
    /// rest carry on; if nothing could be done for one shared reason, that
    /// reason is the error. A phone that cannot do it by itself is
    /// `Unsupported`, before anything is touched, and the paste goes through
    /// the Mac instead (see `transfer::Service::paste_through_mac`).
    pub async fn paste(&self, request: &PasteRequest) -> Result<PasteSummary, Error> {
        if request.names.is_empty() {
            return Ok(Self::nothing_pasted(request));
        }
        let session = self.device.session().await?;
        self.paste_with(request, session.capabilities()).await
    }

    fn nothing_pasted(request: &PasteRequest) -> PasteSummary {
        PasteSummary {
            mode: request.mode,
            done: 0,
            renamed: 0,
            into: normalize(&request.to),
            names: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// `paste`, for a phone with these `capabilities`.
    pub(super) async fn paste_with(
        &self,
        request: &PasteRequest,
        capabilities: &Capabilities,
    ) -> Result<PasteSummary, Error> {
        let mut summary = Self::nothing_pasted(request);
        if !can_do(capabilities, request.mode) {
            return Err(Error::Unsupported);
        }
        let Some(Checked {
            from,
            source,
            items,
            dest,
        }) = self.checked(request).await?
        else {
            return Ok(summary);
        };
        // A phone that can copy or move, but not rename and move around a name
        // that is taken, can still not do this paste if a name is: say so now,
        // while nothing has been touched, so the Mac can do it.
        let names: Vec<&str> = items.iter().map(|item| item.filename.as_str()).collect();
        let existing: Vec<String> = dest.children.iter().map(|c| c.filename.clone()).collect();
        if !can_paste_here(capabilities, request.mode, batch_clashes(&names, &existing)) {
            return Err(Error::Unsupported);
        }

        let other_storage =
            (request.from_storage != request.to_storage).then_some(StorageId(request.to_storage));
        let mut taken: Vec<String> = dest.children.iter().map(|c| c.filename.clone()).collect();
        let source_names: Vec<String> =
            source.children.iter().map(|c| c.filename.clone()).collect();

        let mut moved_folders = Vec::new();
        for item in &items {
            let target = Target {
                mode: request.mode,
                capabilities,
                folder: dest.handle,
                storage: other_storage,
                taken: &taken,
                source_names: &source_names,
                limits: self.limits.clone(),
            };
            let this = Item {
                handle: item.handle,
                name: &item.filename,
                is_folder: item.is_folder(),
            };
            match place_one(&source.storage, &dest.storage, &target, &this).await {
                Ok((handle, final_name)) => {
                    summary.done += 1;
                    summary.renamed += usize::from(final_name != item.filename);
                    taken.push(final_name.clone());
                    summary.names.push(final_name);
                    if request.mode == Paste::Move && item.is_folder() {
                        moved_folders.push(join(&from, &item.filename));
                    }
                    // Keep what is remembered of both folders true, so showing
                    // them again needs no round trip per file; if the phone
                    // won't describe the new item, forget the folder instead.
                    match dest.limits.ask(dest.storage.get_object_info(handle)).await {
                        Ok(info) => self.amend(&dest.key, |kept| kept.added(info.clone())).await,
                        Err(_) => self.forget_listing(&dest.key).await,
                    }
                    if request.mode == Paste::Move {
                        self.amend(&source.key, |kept| kept.removed(&[item.handle]))
                            .await;
                    }
                }
                Err(Refused::NameTaken) => summary.failures.push(FileFailure {
                    label: item.filename.clone(),
                    error: Error::NameTaken(item.filename.clone()),
                }),
                Err(Refused::Phone(err)) if is_about_one_object(&err) => {
                    summary.failures.push(FileFailure {
                        label: item.filename.clone(),
                        error: mapper::to_error(err, false),
                    });
                }
                Err(Refused::Phone(err)) => {
                    self.forget_listing(&source.key).await;
                    self.forget_listing(&dest.key).await;
                    return Err(self.device.handle_error(err).await);
                }
            }
        }

        // Moved folders have new paths, so what is remembered below the old
        // ones, and the path to handle cache, is wrong.
        if !moved_folders.is_empty() {
            self.folders.lock().await.clear();
            for old in moved_folders {
                self.forget_below(&source.key, &old).await;
            }
        }
        if let Some(error) = whole_batch_failure(summary.done, 0, &summary.failures) {
            return Err(error);
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// The call that fails, and the error it fails with.
    type Failure = (&'static str, fn() -> mtp_rs::Error);

    /// A phone that records every call and can be told to fail one.
    struct Fake {
        calls: Mutex<Vec<String>>,
        fail: Option<Failure>,
        /// The call that is made and never answered.
        hang: Option<&'static str>,
        next: AtomicU64,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail: None,
                hang: None,
                next: AtomicU64::new(100),
            }
        }

        fn failing(call: &'static str, error: fn() -> mtp_rs::Error) -> Self {
            Self {
                fail: Some((call, error)),
                ..Self::new()
            }
        }

        /// A phone that never answers `call`.
        fn silent_on(call: &'static str) -> Self {
            Self {
                hang: Some(call),
                ..Self::new()
            }
        }

        /// Never returns when this is the call that gets no answer.
        async fn hang_if(&self, call: &str) {
            if self.hang == Some(call) {
                std::future::pending::<()>().await;
            }
        }

        fn log(&self, call: &str, text: String) -> Result<(), mtp_rs::Error> {
            self.calls.lock().unwrap().push(text);
            match self.fail {
                Some((name, error)) if name == call => Err(error()),
                _ => Ok(()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn fresh(&self) -> ObjectHandle {
            ObjectHandle(self.next.fetch_add(1, Ordering::SeqCst))
        }
    }

    fn storage_note(storage: Option<StorageId>) -> String {
        storage.map_or(String::new(), |s| format!(" on {}", s.0))
    }

    impl Pasting for Fake {
        async fn copy(
            &self,
            handle: ObjectHandle,
            parent: ObjectHandle,
            storage: Option<StorageId>,
        ) -> Result<ObjectHandle, mtp_rs::Error> {
            self.log(
                "copy",
                format!(
                    "copy {} into {}{}",
                    handle.0,
                    parent.0,
                    storage_note(storage)
                ),
            )?;
            self.hang_if("copy").await;
            Ok(self.fresh())
        }

        async fn relocate(
            &self,
            handle: ObjectHandle,
            parent: ObjectHandle,
            storage: Option<StorageId>,
        ) -> Result<(), mtp_rs::Error> {
            self.log(
                "relocate",
                format!(
                    "move {} into {}{}",
                    handle.0,
                    parent.0,
                    storage_note(storage)
                ),
            )?;
            self.hang_if("relocate").await;
            Ok(())
        }

        async fn rename(&self, handle: ObjectHandle, name: &str) -> Result<(), mtp_rs::Error> {
            self.log("rename", format!("rename {} to {name}", handle.0))?;
            self.hang_if("rename").await;
            Ok(())
        }

        async fn make_folder(
            &self,
            parent: Option<ObjectHandle>,
            name: &str,
        ) -> Result<ObjectHandle, mtp_rs::Error> {
            let parent = parent.map_or("root".to_owned(), |p| p.0.to_string());
            self.log("make_folder", format!("make {name} in {parent}"))?;
            self.hang_if("make_folder").await;
            Ok(self.fresh())
        }

        async fn remove_folder(
            &self,
            _: &Limits,
            handle: ObjectHandle,
        ) -> Result<(), mtp_rs::Error> {
            self.log("remove_folder", format!("remove folder {}", handle.0))?;
            self.hang_if("remove_folder").await;
            Ok(())
        }
    }

    const ALL: Capabilities = Capabilities {
        can_upload: true,
        can_delete: true,
        can_rename: true,
        can_move: true,
        can_copy: true,
        can_create_folder: true,
        supports_partial_download: true,
        supports_thumbnails: true,
        supports_events: true,
    };

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// Pastes `a.jpg` (handle 5) into folder 7, where `taken` is already there.
    async fn paste_a(
        phone: &Fake,
        mode: Paste,
        capabilities: &Capabilities,
        taken: &[&str],
        source_names: &[&str],
    ) -> Result<(ObjectHandle, String), Refused> {
        let taken = strings(taken);
        let source_names = strings(source_names);
        let target = Target {
            mode,
            capabilities,
            folder: Some(ObjectHandle(7)),
            storage: None,
            taken: &taken,
            source_names: &source_names,
            limits: Limits::default(),
        };
        let item = Item {
            handle: ObjectHandle(5),
            name: "a.jpg",
            is_folder: false,
        };
        place_one(phone, phone, &target, &item).await
    }

    #[tokio::test]
    async fn copying_to_a_free_name_is_one_copy() {
        let phone = Fake::new();

        let (handle, name) = paste_a(&phone, Paste::Copy, &ALL, &["b.jpg"], &[])
            .await
            .unwrap();

        assert_eq!(phone.calls(), ["copy 5 into 7"]);
        assert_eq!((handle.0, name.as_str()), (100, "a.jpg"));
    }

    #[tokio::test]
    async fn copying_onto_a_taken_name_goes_through_a_scratch_folder_and_never_onto_it() {
        let phone = Fake::new();

        let (handle, name) = paste_a(&phone, Paste::Copy, &ALL, &["a.jpg"], &[])
            .await
            .unwrap();

        assert_eq!(
            phone.calls(),
            [
                "make .open-mtp-client-paste-1 in 7",
                "copy 5 into 100",
                "rename 101 to a (1).jpg",
                "move 101 into 7",
                "remove folder 100",
            ]
        );
        assert_eq!((handle.0, name.as_str()), (101, "a (1).jpg"));
        assert!(
            !phone.calls().iter().any(|call| call == "copy 5 into 7"),
            "the original is never copied onto its own name"
        );
    }

    #[tokio::test]
    async fn the_clash_is_found_ignoring_case() {
        let phone = Fake::new();

        let (_, name) = paste_a(&phone, Paste::Copy, &ALL, &["A.JPG"], &[])
            .await
            .unwrap();

        assert_eq!(name, "a (1).jpg");
    }

    #[tokio::test]
    async fn a_copy_takes_the_first_free_number() {
        let phone = Fake::new();

        let (_, name) = paste_a(
            &phone,
            Paste::Copy,
            &ALL,
            &["a.jpg", "a (1).jpg", "a (2).jpg"],
            &[],
        )
        .await
        .unwrap();

        assert_eq!(name, "a (3).jpg");
    }

    #[tokio::test]
    async fn the_scratch_folder_name_avoids_what_is_there() {
        assert_eq!(scratch_name(&[]), ".open-mtp-client-paste-1");
        assert_eq!(
            scratch_name(&strings(&[
                ".open-mtp-client-paste-1",
                ".OPEN-MTP-CLIENT-PASTE-2"
            ])),
            ".open-mtp-client-paste-3"
        );
    }

    #[tokio::test]
    async fn the_scratch_folder_is_removed_when_the_copy_fails() {
        let phone = Fake::failing("copy", || mtp_rs::Error::Busy);

        let result = paste_a(&phone, Paste::Copy, &ALL, &["a.jpg"], &[]).await;

        assert!(matches!(result, Err(Refused::Phone(mtp_rs::Error::Busy))));
        assert_eq!(
            phone.calls().last().map(String::as_str),
            Some("remove folder 100"),
            "{:?}",
            phone.calls()
        );
    }

    #[tokio::test]
    async fn the_scratch_folder_is_removed_when_the_rename_or_the_move_fails() {
        for failing in ["rename", "relocate"] {
            let phone = Fake::failing(failing, || mtp_rs::Error::AccessDenied);

            let result = paste_a(&phone, Paste::Copy, &ALL, &["a.jpg"], &[]).await;

            assert!(matches!(result, Err(Refused::Phone(_))), "{failing}");
            assert_eq!(
                phone.calls().last().map(String::as_str),
                Some("remove folder 100"),
                "{failing}: {:?}",
                phone.calls()
            );
        }
    }

    #[tokio::test]
    async fn a_phone_that_cannot_make_the_safe_copy_refuses_the_item_and_changes_nothing() {
        for without in ["rename", "move", "folder"] {
            let capabilities = Capabilities {
                can_rename: without != "rename",
                can_move: without != "move",
                can_create_folder: without != "folder",
                ..ALL
            };
            let phone = Fake::new();

            let result = paste_a(&phone, Paste::Copy, &capabilities, &["a.jpg"], &[]).await;

            assert!(
                matches!(result, Err(Refused::NameTaken)),
                "without {without}"
            );
            assert!(
                phone.calls().is_empty(),
                "without {without}: {:?}",
                phone.calls()
            );
        }
    }

    #[tokio::test]
    async fn a_failing_scratch_folder_means_no_copy_is_attempted() {
        let phone = Fake::failing("make_folder", || mtp_rs::Error::AccessDenied);

        let result = paste_a(&phone, Paste::Copy, &ALL, &["a.jpg"], &[]).await;

        assert!(matches!(result, Err(Refused::Phone(_))));
        assert_eq!(phone.calls(), ["make .open-mtp-client-paste-1 in 7"]);
    }

    #[tokio::test]
    async fn moving_to_a_free_name_is_one_move() {
        let phone = Fake::new();

        let (handle, name) = paste_a(&phone, Paste::Move, &ALL, &["b.jpg"], &[])
            .await
            .unwrap();

        assert_eq!(phone.calls(), ["move 5 into 7"]);
        assert_eq!(
            (handle.0, name.as_str()),
            (5, "a.jpg"),
            "a move keeps the handle"
        );
    }

    #[tokio::test]
    async fn moving_onto_a_taken_name_renames_first_to_a_name_free_in_both_places() {
        let phone = Fake::new();

        // The source folder already has `a (1).jpg`, so the new name must skip it.
        let (handle, name) = paste_a(
            &phone,
            Paste::Move,
            &ALL,
            &["a.jpg"],
            &["a.jpg", "a (1).jpg"],
        )
        .await
        .unwrap();

        assert_eq!(phone.calls(), ["rename 5 to a (2).jpg", "move 5 into 7"]);
        assert_eq!((handle.0, name.as_str()), (5, "a (2).jpg"));
    }

    #[tokio::test]
    async fn a_move_that_fails_after_the_rename_puts_the_name_back() {
        let phone = Fake::failing("relocate", || mtp_rs::Error::AccessDenied);

        let result = paste_a(&phone, Paste::Move, &ALL, &["a.jpg"], &["a.jpg"]).await;

        assert!(matches!(
            result,
            Err(Refused::Phone(mtp_rs::Error::AccessDenied))
        ));
        assert_eq!(
            phone.calls(),
            [
                "rename 5 to a (1).jpg",
                "move 5 into 7",
                "rename 5 to a.jpg"
            ]
        );
    }

    #[tokio::test]
    async fn a_move_that_may_have_happened_is_not_renamed_back_onto_a_taken_name() {
        // A timeout can mean the phone moved the item and the reply was lost.
        // Then `a.jpg` is taken in the target by another file, and asking the
        // phone to rename the item there to `a.jpg` is the clash to avoid.
        for lost in [|| mtp_rs::Error::Timeout, || mtp_rs::Error::Disconnected] {
            let phone = Fake::failing("relocate", lost);

            let result = paste_a(&phone, Paste::Move, &ALL, &["a.jpg"], &["a.jpg"]).await;

            assert!(matches!(result, Err(Refused::Phone(_))));
            assert_eq!(
                phone.calls(),
                ["rename 5 to a (1).jpg", "move 5 into 7"],
                "no second rename"
            );
        }
    }

    #[tokio::test]
    async fn a_rename_that_fails_stops_the_move_before_it_starts() {
        let phone = Fake::failing("rename", || mtp_rs::Error::AccessDenied);

        let result = paste_a(&phone, Paste::Move, &ALL, &["a.jpg"], &["a.jpg"]).await;

        assert!(matches!(result, Err(Refused::Phone(_))));
        assert_eq!(phone.calls(), ["rename 5 to a (1).jpg"]);
    }

    #[tokio::test]
    async fn a_phone_that_cannot_rename_refuses_a_clashing_move() {
        let capabilities = Capabilities {
            can_rename: false,
            ..ALL
        };
        let phone = Fake::new();

        let result = paste_a(&phone, Paste::Move, &capabilities, &["a.jpg"], &[]).await;

        assert!(matches!(result, Err(Refused::NameTaken)));
        assert!(phone.calls().is_empty());
    }

    #[tokio::test]
    async fn a_phone_that_cannot_rename_still_moves_without_a_clash() {
        let capabilities = Capabilities {
            can_rename: false,
            can_create_folder: false,
            ..ALL
        };
        let phone = Fake::new();

        assert!(
            paste_a(&phone, Paste::Move, &capabilities, &[], &[])
                .await
                .is_ok()
        );
        assert!(
            paste_a(&phone, Paste::Copy, &capabilities, &[], &[])
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn another_storage_is_named_in_the_copy_and_the_move_but_not_inside_the_target() {
        let taken = strings(&["a.jpg"]);
        let target = Target {
            mode: Paste::Copy,
            capabilities: &ALL,
            folder: None,
            storage: Some(StorageId(2)),
            taken: &taken,
            source_names: &[],
            limits: Limits::default(),
        };
        let item = Item {
            handle: ObjectHandle(5),
            name: "a.jpg",
            is_folder: false,
        };
        let phone = Fake::new();

        place_one(&phone, &phone, &target, &item).await.unwrap();

        // The copy into the scratch folder crosses storages; moving the copy up
        // inside the target storage does not; and the root is `ROOT`.
        let root = ObjectHandle::ROOT.0;
        assert_eq!(
            phone.calls(),
            [
                "make .open-mtp-client-paste-1 in root".to_owned(),
                "copy 5 into 100 on 2".to_owned(),
                "rename 101 to a (1).jpg".to_owned(),
                format!("move 101 into {root}"),
                "remove folder 100".to_owned(),
            ]
        );
        let free = Fake::new();
        let target = Target {
            taken: &[],
            mode: Paste::Move,
            ..target
        };
        place_one(&free, &free, &target, &item).await.unwrap();
        assert_eq!(free.calls(), [format!("move 5 into {root} on 2")]);
    }

    #[tokio::test]
    async fn folders_get_the_number_at_the_end_of_their_name() {
        let phone = Fake::new();
        let taken = strings(&["Trip"]);
        let target = Target {
            mode: Paste::Move,
            capabilities: &ALL,
            folder: Some(ObjectHandle(7)),
            storage: None,
            taken: &taken,
            source_names: &[],
            limits: Limits::default(),
        };
        let item = Item {
            handle: ObjectHandle(5),
            name: "Trip",
            is_folder: true,
        };

        let (_, name) = place_one(&phone, &phone, &target, &item).await.unwrap();

        assert_eq!(name, "Trip (1)");
    }

    #[test]
    fn copying_and_moving_need_the_phone_to_say_it_can() {
        let none = Capabilities::default();
        let copy_only = Capabilities {
            can_copy: true,
            ..none
        };
        let move_only = Capabilities {
            can_move: true,
            ..none
        };

        assert!(!can_do(&none, Paste::Copy) && !can_do(&none, Paste::Move));
        assert!(can_do(&copy_only, Paste::Copy) && !can_do(&copy_only, Paste::Move));
        assert!(!can_do(&move_only, Paste::Copy) && can_do(&move_only, Paste::Move));
    }

    #[test]
    fn a_clash_needs_rename_and_for_a_copy_also_move_and_folders() {
        let only = |can_rename, can_move, can_create_folder| Capabilities {
            can_rename,
            can_move,
            can_create_folder,
            ..ALL
        };

        assert!(can_clear_a_clash(&ALL, Paste::Copy));
        assert!(can_clear_a_clash(&ALL, Paste::Move));
        assert!(!can_clear_a_clash(&only(false, true, true), Paste::Copy));
        assert!(!can_clear_a_clash(&only(true, false, true), Paste::Copy));
        assert!(!can_clear_a_clash(&only(true, true, false), Paste::Copy));
        assert!(
            can_clear_a_clash(&only(true, false, false), Paste::Move),
            "a move only renames first"
        );
        assert!(!can_clear_a_clash(&only(false, true, true), Paste::Move));
    }

    #[test]
    fn a_phone_pastes_by_itself_if_it_can_do_the_whole_thing_it_is_asked() {
        let phone = |can_copy, can_move, can_rename| Capabilities {
            can_copy,
            can_move,
            can_rename,
            ..ALL
        };

        // Nothing clashes: the phone's own copy or move is enough.
        assert!(can_paste_here(
            &phone(true, false, false),
            Paste::Copy,
            false
        ));
        assert!(can_paste_here(
            &phone(false, true, false),
            Paste::Move,
            false
        ));
        assert!(!can_paste_here(
            &phone(false, true, true),
            Paste::Copy,
            false
        ));
        assert!(!can_paste_here(
            &phone(true, false, true),
            Paste::Move,
            false
        ));
        // Something clashes: it also has to be able to go round the clash.
        assert!(can_paste_here(&ALL, Paste::Copy, true));
        assert!(!can_paste_here(
            &phone(true, true, false),
            Paste::Copy,
            true
        ));
        assert!(!can_paste_here(
            &phone(true, false, true),
            Paste::Copy,
            true
        ));
        assert!(!can_paste_here(
            &phone(true, true, false),
            Paste::Move,
            true
        ));
        assert!(can_paste_here(&phone(true, true, true), Paste::Move, true));
    }

    #[test]
    fn a_batch_clashes_with_the_target_and_with_itself_ignoring_case() {
        let taken = strings(&["a.jpg", "Trip"]);

        assert!(!batch_clashes(&["b.jpg", "c.jpg"], &taken));
        assert!(
            batch_clashes(&["b.jpg", "A.JPG"], &taken),
            "case is the phone's business too"
        );
        assert!(batch_clashes(&["Trip"], &taken));
        assert!(
            batch_clashes(&["b.jpg", "B.JPG"], &taken),
            "the second meets the first once it is placed"
        );
        assert!(!batch_clashes(&[], &taken));
        assert!(!batch_clashes(&["a.jpg"], &[]));
    }

    // ---- a phone that never answers one of them ----

    const QUICK: std::time::Duration = std::time::Duration::from_millis(40);
    const SLOW: std::time::Duration = std::time::Duration::from_millis(240);

    /// `paste_a` against a phone that never answers `call`, with `QUICK` and
    /// `SLOW` for the limits, and how long it took to end.
    async fn silent_paste(
        call: &'static str,
        mode: Paste,
        on_another_storage: bool,
        taken: &[&str],
    ) -> (
        Result<(ObjectHandle, String), Refused>,
        std::time::Duration,
        Vec<String>,
    ) {
        silent_paste_of(call, mode, on_another_storage, taken, false).await
    }

    /// `silent_paste`, of a folder if `is_folder`.
    async fn silent_paste_of(
        call: &'static str,
        mode: Paste,
        on_another_storage: bool,
        taken: &[&str],
        is_folder: bool,
    ) -> (
        Result<(ObjectHandle, String), Refused>,
        std::time::Duration,
        Vec<String>,
    ) {
        let phone = Fake::silent_on(call);
        let taken = strings(taken);
        let target = Target {
            mode,
            capabilities: &ALL,
            folder: Some(ObjectHandle(7)),
            storage: on_another_storage.then_some(StorageId(2)),
            taken: &taken,
            source_names: &[],
            limits: Limits::new(QUICK, SLOW),
        };
        let item = Item {
            handle: ObjectHandle(5),
            name: "a.jpg",
            is_folder,
        };
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(SLOW * 10, place_one(&phone, &phone, &target, &item))
            .await
            .expect("a phone that never answers must not hang a paste");
        (result, started.elapsed(), phone.calls())
    }

    fn timed_out(result: &Result<(ObjectHandle, String), Refused>) -> bool {
        matches!(result, Err(Refused::Phone(mtp_rs::Error::Timeout)))
    }

    #[tokio::test]
    async fn a_copy_nobody_answers_ends_at_the_long_limit_not_the_quick_one() {
        let (result, took, _) = silent_paste("copy", Paste::Copy, false, &[]).await;

        assert!(timed_out(&result), "{result:?}");
        assert!(took >= SLOW, "a copy may take minutes: {took:?}");
    }

    #[tokio::test]
    async fn a_move_within_the_storage_is_quick_and_to_another_one_is_long() {
        let (within, took, _) = silent_paste("relocate", Paste::Move, false, &[]).await;
        assert!(timed_out(&within), "{within:?}");
        assert!(
            took >= QUICK && took < SLOW,
            "only a change of parent: {took:?}"
        );

        let (across, took, _) = silent_paste("relocate", Paste::Move, true, &[]).await;
        assert!(timed_out(&across), "{across:?}");
        assert!(took >= SLOW, "the phone copies and deletes first: {took:?}");
    }

    #[tokio::test]
    async fn the_quick_requests_of_a_clashing_copy_each_end_at_the_quick_limit() {
        let (making, took, calls) =
            silent_paste("make_folder", Paste::Copy, false, &["a.jpg"]).await;
        assert!(timed_out(&making), "{making:?}");
        assert!(took >= QUICK && took < SLOW, "{took:?}");
        assert_eq!(
            calls,
            ["make .open-mtp-client-paste-1 in 7"],
            "nothing was copied into a folder that was never made"
        );

        let (renaming, took, calls) = silent_paste("rename", Paste::Copy, false, &["a.jpg"]).await;
        assert!(timed_out(&renaming), "{renaming:?}");
        assert!(took >= QUICK && took < SLOW, "{took:?}");
        assert_eq!(
            calls,
            [
                "make .open-mtp-client-paste-1 in 7",
                "copy 5 into 100",
                "rename 101 to a (1).jpg",
                "remove folder 100",
            ],
            "the copy is not moved out, and the scratch folder is asked to go"
        );
    }

    #[tokio::test]
    async fn a_clashing_copy_that_hangs_inside_the_scratch_folder_still_tidies_up_what_it_can() {
        let (result, took, calls) = silent_paste("copy", Paste::Copy, false, &["a.jpg"]).await;

        assert!(timed_out(&result), "{result:?}");
        assert!(took >= SLOW);
        assert_eq!(
            calls.last().map(String::as_str),
            Some("remove folder 100"),
            "the scratch folder is still asked to go: {calls:?}"
        );
    }

    #[tokio::test]
    async fn tidying_up_after_a_copy_is_not_waited_on_for_ever_and_does_not_undo_it() {
        let (result, took, calls) =
            silent_paste("remove_folder", Paste::Copy, false, &["a.jpg"]).await;

        assert!(result.is_ok(), "the copy is made: {result:?}");
        assert!(took >= SLOW && took < SLOW * 5, "{took:?}");
        assert!(calls.iter().any(|c| c.starts_with("remove folder")));
    }

    #[tokio::test]
    async fn a_clashing_move_that_hangs_leaves_the_item_under_its_new_name() {
        // A request nobody answered may have happened; renaming back could land
        // on a name that is taken. It is not tried.
        let (result, _, calls) = silent_paste("relocate", Paste::Move, false, &["a.jpg"]).await;

        assert!(timed_out(&result), "{result:?}");
        assert_eq!(
            calls,
            ["rename 5 to a (1).jpg", "move 5 into 7"],
            "no rename back"
        );
        let (renaming, took, _) = silent_paste("rename", Paste::Move, false, &["a.jpg"]).await;
        assert!(timed_out(&renaming) && took < SLOW, "{renaming:?} {took:?}");
    }

    #[tokio::test]
    async fn renaming_or_moving_a_folder_gets_the_long_limit_because_the_phone_rewrites_what_is_in_it()
     {
        // Within the storage, a file's move is quick; a folder's is not.
        let (file, took, _) = silent_paste_of("relocate", Paste::Move, false, &[], false).await;
        assert!(timed_out(&file) && took < SLOW, "{took:?}");
        let (folder, took, _) = silent_paste_of("relocate", Paste::Move, false, &[], true).await;
        assert!(timed_out(&folder) && took >= SLOW, "{took:?}");

        // So is renaming one first, when its name is taken in the target.
        let (file, took, _) =
            silent_paste_of("rename", Paste::Move, false, &["a.jpg"], false).await;
        assert!(timed_out(&file) && took < SLOW, "{took:?}");
        let (folder, took, _) =
            silent_paste_of("rename", Paste::Move, false, &["a.jpg"], true).await;
        assert!(timed_out(&folder) && took >= SLOW, "{took:?}");

        // And the rename of a copy of a folder made beside its original.
        let (folder, took, _) =
            silent_paste_of("rename", Paste::Copy, false, &["a.jpg"], true).await;
        assert!(timed_out(&folder) && took >= SLOW, "{took:?}");
    }
}
