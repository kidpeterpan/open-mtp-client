//! Sending each file back to the phone as soon as it has been read from it.
//!
//! A phone that cannot copy or move by itself (`through_mac.rs`) is copied
//! through the Mac. MTP cannot do that as a stream: a write to the phone holds
//! its one session until the whole file is in, so nothing can be read from the
//! phone meanwhile (a read started from inside the write's data never comes
//! back). What it can do is one file at a time: read a file into the private
//! scratch folder, send it, delete it, take the next. The Mac then needs room
//! for the biggest file, not for the whole selection.
//!
//! `download_run` plans and reads as it always has and calls an `Onward` after
//! each folder and file; `Relay` is the one that sends them to the phone. It
//! makes each phone folder when the walk reaches it and holds the handles it
//! made, so a file goes into the folder this copy created and never into one
//! that merely has the same name, and it names each selected item once, as
//! Finder's "keep both" would, so nothing already on the phone is overwritten.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use mtp_rs::mtp::ObjectHandle;

use super::room::{Room, phone_room};
use super::service::{Failure, phone_answers};
use super::upload::{Children, Delivered, LocalNode, Sender};
use crate::dto::{Cancel, TransferProgress};
use crate::service::device;
use crate::service::explorer::{self, Folder, Limits};

/// What a copy does with each file once it has arrived on the Mac, when it goes
/// on to the phone (see the module docs). A plain download has none.
pub(super) trait Onward: Send {
    /// The phone already has this file as it is, so there is nothing to read
    /// and nothing to send: a retried move finds what an earlier one sent.
    fn has(&mut self, relative: &Path, size: u64, modified: Option<SystemTime>) -> bool;

    /// A folder was made on the Mac at `relative` (from the scratch folder).
    fn folder(&mut self, relative: &Path) -> impl Future<Output = Result<(), Failure>> + Send;

    /// A file has arrived at `arrived`, `relative` being where it goes. It is
    /// deleted from the Mac when this returns, whatever the result.
    fn file(
        &mut self,
        relative: &Path,
        arrived: &Path,
    ) -> impl Future<Output = Result<(), Failure>> + Send;

    /// The room where the files go on, when it can be told before they do.
    fn room(&self) -> Option<Room> {
        None
    }
}

/// No onward stage: files stay where they were put.
pub(super) struct Nowhere;

impl Onward for Nowhere {
    fn has(&mut self, _: &Path, _: u64, _: Option<SystemTime>) -> bool {
        false
    }

    async fn folder(&mut self, _: &Path) -> Result<(), Failure> {
        Ok(())
    }

    async fn file(&mut self, _: &Path, _: &Path) -> Result<(), Failure> {
        Ok(())
    }
}

/// How one selected item fared on the phone.
#[derive(Debug, Default)]
struct Item {
    /// What it is called on the phone.
    name: String,
    /// Something in it failed to send.
    failed: bool,
    /// Something in it could not be read back from the phone.
    unconfirmed: bool,
}

/// Where something goes on the phone: the folder, the name, and the folder
/// already there that it merges into, if it is one.
struct Placed {
    parent: Option<ObjectHandle>,
    name: String,
    merge_into: Option<ObjectHandle>,
}

/// Sends what arrives to a folder on the phone.
pub(super) struct Relay<'a, P> {
    sender: Sender<'a, P>,
    /// The folder the items are pasted into.
    into: &'a Folder,
    limits: &'a Limits,
    /// A retried move finds files an earlier attempt sent; a copy never does.
    moving: bool,
    /// A retry of a move that stopped part way: a folder merges into the one of
    /// the same name (which the first try made) instead of becoming `name (1)`.
    /// Nothing there is replaced: a different file of the same name gets a
    /// numbered name beside it.
    resuming: bool,
    /// What each phone folder that was already there holds, and the names taken
    /// in it since: the folder pasted into, and any folder merged into. A
    /// folder this copy made is empty and needs no entry.
    contents: HashMap<Option<u64>, Children>,
    /// The selected items, by the name they have on the Mac.
    items: HashMap<String, Item>,
    /// The phone folder made (or merged into) for each Mac folder, by its place
    /// in the scratch folder.
    folders: HashMap<PathBuf, ObjectHandle>,
    already_there: usize,
    renamed: usize,
    merged: usize,
    first_item: Option<String>,
}

fn key(folder: Option<ObjectHandle>) -> Option<u64> {
    folder.map(|handle| handle.0)
}

/// The selected item a path in the scratch folder belongs to.
fn item_of(relative: &Path) -> Option<String> {
    relative
        .components()
        .next()
        .map(|first| first.as_os_str().to_string_lossy().into_owned())
}

impl<'a, P: Fn(TransferProgress) + Sync> Relay<'a, P> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        into: &'a Folder,
        limits: &'a Limits,
        cancel: &'a Cancel,
        progress: &'a P,
        started: Instant,
        moving: bool,
        resuming: bool,
        stall_limit: std::time::Duration,
        device: &device::Service,
    ) -> Self {
        Self {
            sender: Sender::new(
                &into.storage,
                limits,
                cancel,
                progress,
                started,
                moving,
                stall_limit,
                device.activity(),
            ),
            into,
            limits,
            moving,
            resuming,
            contents: HashMap::from([(key(into.handle), Children::new(into.children.clone()))]),
            items: HashMap::new(),
            folders: HashMap::new(),
            already_there: 0,
            renamed: 0,
            merged: 0,
            first_item: None,
        }
    }

    /// Files sent, and the bytes in them.
    pub(super) fn sent(&self) -> (usize, u64) {
        (self.sender.state.files_done, self.sender.state.bytes_done)
    }

    /// Files the phone already had, selected items that were put under a new
    /// name because theirs was taken, and selected folders that were merged
    /// into the one of the same name.
    pub(super) fn counts(&self) -> (usize, usize, usize) {
        (self.already_there, self.renamed, self.merged)
    }

    /// The first selected item, as it is named on the phone.
    pub(super) fn first_item(&self) -> Option<String> {
        self.first_item.clone()
    }

    /// How each of `items` (names on the Mac) fared, for a move to decide
    /// whose originals may go: an item is confirmed if everything sent was
    /// read back, and one that was never reached is not.
    pub(super) fn delivered(&self, items: &[String]) -> Vec<Delivered> {
        items
            .iter()
            .filter_map(|name| self.items.get(name).map(|item| (name, item)))
            .map(|(name, item)| Delivered {
                name: name.clone(),
                failed: item.failed,
                skipped: false,
                confirmed: !item.failed && !item.unconfirmed,
                node: Arc::new(LocalNode::standing_for(name)),
            })
            .collect()
    }

    /// The phone folder (as a key into `contents`) that holds what is at
    /// `relative`, and its name there.
    fn parent_of(&self, relative: &Path) -> Option<(Option<ObjectHandle>, String)> {
        let name = relative.file_name()?.to_string_lossy().into_owned();
        if relative.components().count() == 1 {
            return Some((self.into.handle, name));
        }
        let parent = self.folders.get(relative.parent()?).copied()?;
        Some((Some(parent), name))
    }

    /// The phone folder and name for what is at `relative` in the scratch
    /// folder. A selected item gets its name now, once: its own if free, else a
    /// numbered one; a retry puts a folder into the one that is there.
    fn place(&mut self, relative: &Path, is_folder: bool) -> Result<Placed, Failure> {
        let (Some(item), Some((parent, name))) = (item_of(relative), self.parent_of(relative))
        else {
            return Err(Failure::UnsafeName(relative.to_string_lossy().into_owned()));
        };
        let top = relative.components().count() == 1;
        if top && let Some(known) = self.items.get(&item) {
            return Ok(Placed {
                parent,
                name: known.name.clone(),
                merge_into: None,
            });
        }
        // A folder this copy made has nothing in it to clash with.
        let Some(children) = self.contents.get_mut(&key(parent)) else {
            return Ok(Placed {
                parent,
                name,
                merge_into: None,
            });
        };
        let there = children
            .named(&name)
            .filter(|existing| self.resuming && is_folder && existing.is_folder())
            .map(|existing| (existing.handle, existing.filename.clone()));
        let (chosen, merge_into) = match there {
            Some((handle, filename)) => (filename, Some(handle)),
            None => (children.claim(&name, is_folder), None),
        };
        if top {
            if merge_into.is_some() {
                self.merged += 1;
            } else if chosen != name {
                self.renamed += 1;
            }
            self.first_item.get_or_insert_with(|| chosen.clone());
            self.items.insert(
                item,
                Item {
                    name: chosen.clone(),
                    ..Item::default()
                },
            );
        }
        Ok(Placed {
            parent,
            name: chosen,
            merge_into,
        })
    }

    /// Notes against the selected item at `relative` that it did not go well.
    fn spoil(&mut self, relative: &Path) {
        if let Some(item) = item_of(relative).and_then(|item| self.items.get_mut(&item)) {
            item.failed = true;
        }
    }
}

impl<P: Fn(TransferProgress) + Sync + Send> Onward for Relay<'_, P> {
    /// The phone storage pasted into. Not for a retried move: what an earlier
    /// try sent is found there and not sent again, so the plan says more than
    /// will go.
    fn room(&self) -> Option<Room> {
        let info = self.into.storage.info();
        (!self.resuming).then(|| phone_room(info.free_space, &info.description))
    }

    fn has(&mut self, relative: &Path, size: u64, modified: Option<SystemTime>) -> bool {
        if !self.moving {
            return false;
        }
        let Some((parent, name)) = self.parent_of(relative) else {
            return false;
        };
        let Some(on_phone) = self
            .contents
            .get(&key(parent))
            .and_then(|children| children.copy_of(&name, size, modified))
        else {
            return false;
        };
        let on_phone = on_phone.filename.clone();
        if relative.components().count() == 1
            && let Some(item) = item_of(relative)
        {
            self.first_item.get_or_insert_with(|| on_phone.clone());
            self.items.insert(
                item,
                Item {
                    name: on_phone,
                    ..Item::default()
                },
            );
        }
        self.already_there += 1;
        true
    }

    async fn folder(&mut self, relative: &Path) -> Result<(), Failure> {
        let placed = self.place(relative, true)?;
        let made = match placed.merge_into {
            // What is in it is read once, to know what it already has.
            Some(handle) => {
                let existing = phone_answers(
                    &self.sender.heard,
                    self.sender.stall_limit,
                    explorer::read_children(self.sender.storage, Some(handle), self.limits),
                )
                .await
                .and_then(|read| read.map_err(Failure::Device));
                existing.map(|existing| {
                    self.contents
                        .insert(key(Some(handle)), Children::new(existing));
                    Some(handle)
                })
            }
            None => phone_answers(
                &self.sender.heard,
                self.sender.stall_limit,
                self.sender
                    .storage
                    .create_folder(placed.parent, &placed.name),
            )
            .await
            .and_then(|made| made.map_err(Failure::Device))
            .map(Some),
        };
        match made {
            Ok(Some(handle)) => {
                self.folders.insert(relative.to_path_buf(), handle);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(failure) => {
                self.spoil(relative);
                Err(failure)
            }
        }
    }

    async fn file(&mut self, relative: &Path, arrived: &Path) -> Result<(), Failure> {
        let metadata = tokio::fs::metadata(arrived)
            .await
            .map_err(|err| Failure::Mac(err, arrived.to_path_buf()))?;
        let placed = self.place(relative, false)?;
        let node = LocalNode {
            path: arrived.to_path_buf(),
            name: placed.name.clone(),
            is_folder: false,
            size: metadata.len(),
            modified: metadata.modified().ok(),
            children: Vec::new(),
            left_out: 0,
        };
        let unconfirmed_before = self.sender.unconfirmed;
        let label = relative.to_string_lossy().into_owned();
        match self
            .sender
            .send_file(&node, placed.parent, &placed.name, label)
            .await
        {
            Ok(()) => {
                if self.sender.unconfirmed > unconfirmed_before
                    && let Some(item) = item_of(relative).and_then(|item| self.items.get_mut(&item))
                {
                    item.unconfirmed = true;
                }
                Ok(())
            }
            Err(failure) => {
                self.spoil(relative);
                Err(failure)
            }
        }
    }
}
