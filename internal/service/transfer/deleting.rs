//! Phone deletes that keep a copy in the Mac's Trash: each
//! selected item is copied down first, as a move to the Mac copies (checked,
//! forced to the disk), into a private scratch folder; an item whose every
//! file arrived whole goes to the Trash, and only then are its originals
//! deleted from the phone, as a move removes them (looked up again, folders
//! only once empty, so a file that came after the copy stays). An item whose
//! copy failed, or that the Trash refused, stays on the phone. A cancel or a
//! lost phone while copying deletes nothing.

use super::relay::Nowhere;
use super::service::{Outcome, Service, Step};
use crate::dto::{
    Cancel, Conflict, DeleteRequest, Direction, DownloadRequest, Error, Kept, TransferProgress,
    TransferSummary, Why,
};

impl Service {
    /// Deletes `request.names` from the phone, each only after a whole copy
    /// of it is in the Mac's Trash (see the module docs).
    pub async fn delete_keeping_copy(
        &self,
        request: &DeleteRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let Some(trash) = self.trash.clone() else {
            return Err(Error::Unsupported);
        };
        // Private to this user, and gone whatever happens: what didn't reach
        // the Trash is still on the phone.
        let scratch = tempfile::Builder::new()
            .prefix("open-mtp-client-deleting")
            .tempdir()
            .map_err(|err| crate::service::local::mapper::to_error(&err, "a scratch folder"))?;
        let download = DownloadRequest {
            storage_id: request.storage_id,
            source: request.folder.clone(),
            names: request.names.clone(),
            destination: scratch.path().to_string_lossy().into_owned(),
        };
        let mut done = self
            .download_run_onward_with::<Nowhere>(
                true,
                &download,
                Conflict::KeepBoth,
                cancel,
                &progress,
                None,
            )
            .await?;

        let mut kept = Vec::new();
        for (top, name) in done.plan.items.clone().iter().enumerate() {
            let steps: Vec<usize> = done
                .plan
                .origins
                .iter()
                .enumerate()
                .filter(|(_, origin)| origin.top == top)
                .map(|(index, _)| index)
                .collect();
            let whole = steps
                .iter()
                .all(|&index| done.outcomes[index] == Outcome::Verified);
            let copy = steps
                .iter()
                .find(|&&index| done.plan.origins[index].is_top())
                .map(|&index| match &done.plan.steps[index] {
                    Step::Folder(path) | Step::File { target: path, .. } => path.clone(),
                });
            let trashed = match (whole, copy) {
                (true, Some(copy)) => {
                    let trash = trash.clone();
                    tokio::task::spawn_blocking(move || trash(&copy))
                        .await
                        .map_err(std::io::Error::other)
                        .and_then(|result| result)
                        .map_err(|err| {
                            Why::Refused(crate::service::local::mapper::to_error(&err, name))
                        })
                }
                _ => Err(Why::NotCopied),
            };
            if let Err(why) = trashed {
                // Nothing of it leaves the phone.
                for &index in &steps {
                    done.outcomes[index] = Outcome::NotCopied;
                }
                kept.push(Kept {
                    label: name.clone(),
                    why,
                });
            }
        }

        let mut fates = vec![None; done.plan.steps.len()];
        let mut removal = self
            .remove_originals(
                &done.storage,
                &download,
                &done.plan,
                &done.outcomes,
                cancel,
                &progress,
                &mut fates,
            )
            .await;
        // The items kept whole are said once each, not file by file.
        removal.kept.splice(0..0, kept);
        Ok(TransferSummary {
            direction: Direction::Download,
            destination: request.folder.clone(),
            removal: Some(removal),
            report: Vec::new(),
            report_left_out: 0,
            arrival: None,
            ..done.summary
        })
    }
}
