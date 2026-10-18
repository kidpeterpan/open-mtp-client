//! The requests as the phone takes them, with no limit of their own.
//!
//! These are the two seams (`Objects` for deleting, `Pasting` for copying and
//! moving) as the real phone implements them. Whoever calls them bounds each
//! request (`delete_object`, `place_one`: see `bounded.rs`), and the tests do
//! it with a phone that never answers. Nothing else in this module may talk to
//! the phone without a limit; a test (`every_request_in_the_explorer_has_a_limit`)
//! reads the source and holds it to that, and this file is the one exception.

use mtp_rs::StorageId;
use mtp_rs::mtp::{ObjectHandle, Storage};

use super::bounded::Limits;
use super::paste::Pasting;
use super::service::{Objects, delete_object};

impl Objects for Storage {
    async fn delete(&self, handle: ObjectHandle) -> Result<(), mtp_rs::Error> {
        Storage::delete(self, handle).await
    }

    async fn children(
        &self,
        handle: ObjectHandle,
    ) -> Result<Vec<(ObjectHandle, bool)>, mtp_rs::Error> {
        let children = self.list_objects(Some(handle)).await?;
        Ok(children
            .iter()
            .map(|child| (child.handle, child.is_folder()))
            .collect())
    }
}

impl Pasting for Storage {
    async fn copy(
        &self,
        handle: ObjectHandle,
        parent: ObjectHandle,
        storage: Option<StorageId>,
    ) -> Result<ObjectHandle, mtp_rs::Error> {
        self.copy_object(handle, parent, storage).await
    }

    async fn relocate(
        &self,
        handle: ObjectHandle,
        parent: ObjectHandle,
        storage: Option<StorageId>,
    ) -> Result<(), mtp_rs::Error> {
        self.move_object(handle, parent, storage).await
    }

    async fn rename(&self, handle: ObjectHandle, name: &str) -> Result<(), mtp_rs::Error> {
        Storage::rename(self, handle, name).await
    }

    async fn make_folder(
        &self,
        parent: Option<ObjectHandle>,
        name: &str,
    ) -> Result<ObjectHandle, mtp_rs::Error> {
        self.create_folder(parent, name).await
    }

    async fn remove_folder(
        &self,
        limits: &Limits,
        handle: ObjectHandle,
    ) -> Result<(), mtp_rs::Error> {
        delete_object(self, limits, handle, true).await
    }
}
