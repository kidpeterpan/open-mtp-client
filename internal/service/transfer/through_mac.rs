//! Copying and moving inside the phone when the phone cannot do it itself.
//!
//! Many Android phones answer neither CopyObject nor MoveObject, so a paste could
//! simply be refused. This goes the long way round: the items are copied
//! down into a private scratch folder on the Mac, sent back up into the
//! target folder under free names (never over anything), and the scratch
//! folder is thrown away. A move then removes an item's originals only once
//! every file of that item has been read back from the phone, with the same
//! care as a move to the Mac (`remove_originals`).
//!
//! Disk: the whole selection sits in the scratch folder at once, so the Mac
//! needs room for it; when it does not, the copy stops with "disk is full",
//! the scratch folder goes, and nothing on the phone has changed.

use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use super::Service;
use super::relay::Relay;
use super::service::Outcome;
use super::upload::Delivered;
use crate::dto::{
    Cancel, Direction, DownloadRequest, Error, Kept, Paste, PasteRequest, Removal,
    TransferProgress, TransferSummary, Why,
};
use crate::service::local;

impl Service {
    /// Copies or moves `request.names` from one phone folder into another by
    /// way of the Mac. The same checks as the phone's own paste apply first
    /// (a folder into itself, a move into the folder it is in, a read-only
    /// target), and a name taken in the target is never overwritten: the item
    /// gets `name (1)`. For a move, an item's originals are removed only if the
    /// item was copied and read back whole; otherwise it stays, and the
    /// summary says so.
    pub async fn paste_through_mac(
        &self,
        request: &PasteRequest,
        resuming: bool,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        self.through_mac_in(&std::env::temp_dir(), request, resuming, cancel, &progress)
            .await
    }

    /// `paste_through_mac`, with the scratch folder made inside `base`.
    pub(super) async fn through_mac_in(
        &self,
        base: &Path,
        request: &PasteRequest,
        resuming: bool,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) -> Result<TransferSummary, Error> {
        let moving = request.mode == Paste::Move;
        let mut summary = TransferSummary {
            direction: Direction::Upload,
            destination: request.to.clone(),
            removal: moving.then(Removal::default),
            through_mac: true,
            ..TransferSummary::default()
        };
        if !self.explorer.check_paste(request).await? {
            return Ok(summary);
        }

        // Whatever happens next, the scratch folder goes when this returns.
        let scratch = tempfile::Builder::new()
            .prefix("open-mtp-client-")
            .tempdir_in(base)
            .map_err(|err| local::mapper::to_error(&err, &base.to_string_lossy()))?;
        // It holds copies of the user's phone files: nobody else's business.
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|err| local::mapper::to_error(&err, &base.to_string_lossy()))?;
        }
        let scratch_path = scratch.path().to_string_lossy().into_owned();
        // The folder the items go into is read once, not once per file.
        let into = self
            .explorer
            .open_folder_confirmed(request.to_storage, &request.to, &request.names, false)
            .await?;
        into.ensure_writable()?;

        // Reading and sending alternate, a file at a time, so they report as
        // one: the totals are the selection's, and what is done is half of what
        // has been read plus half of what has been sent.
        let meter = Mutex::new(Meter::default());
        let via = |mut report: TransferProgress| {
            report.through_mac = true;
            report.moving = moving;
            meter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .fold_in(&mut report);
            progress(report);
        };
        // Removing the originals is one stage with its own counts.
        let removing = |mut report: TransferProgress| {
            report.through_mac = true;
            report.moving = moving;
            progress(report);
        };

        let down_request = DownloadRequest {
            storage_id: request.from_storage,
            source: request.from.clone(),
            names: request.names.clone(),
            destination: scratch_path,
        };
        let mut relay = Relay::new(
            &into,
            &self.limits,
            cancel,
            &via,
            Instant::now(),
            // A copy always makes another one; only a move may find the very
            // file already there (an earlier attempt's) and say so. A move
            // also reads every file back before its original may go.
            moving,
            resuming && moving,
            self.stall_limit,
            &self.device,
        );
        let down = self
            .download_run_onward(moving, &down_request, cancel, &via, Some(&mut relay))
            .await;
        // Whatever happened, the phone's folder is not as it was remembered.
        self.explorer
            .forget_listings_below(request.to_storage, &request.to)
            .await;
        let down = down?;

        let (files, bytes) = relay.sent();
        let (already_there, renamed, merged) = relay.counts();
        summary.failures = down.summary.failures.clone();
        summary.files = files;
        summary.bytes = bytes;
        summary.renamed = renamed;
        summary.merged = merged;
        summary.already_there = already_there;
        summary.first_item = relay.first_item();

        if moving {
            let delivered = relay.delivered(&down.plan.items);
            let mut outcomes = down.outcomes.clone();
            let good =
                items_that_may_go(&down.plan.items, &down.plan.origins, &outcomes, &delivered);
            for (outcome, origin) in outcomes.iter_mut().zip(&down.plan.origins) {
                if !good[origin.top] {
                    *outcome = Outcome::NotCopied;
                }
            }
            let mut removal = self
                .remove_originals(
                    &down.storage,
                    &down_request,
                    &down.plan,
                    &outcomes,
                    cancel,
                    &removing,
                    &mut [],
                )
                .await;
            // An item that stayed with nothing failed in it stayed because its
            // copy could not be confirmed: say so once, not for every file.
            for (index, item) in down.plan.items.iter().enumerate() {
                if !good[index] && !has_failure(item, &summary.failures, &delivered) {
                    removal.kept.push(Kept {
                        label: item.clone(),
                        why: Why::Unchecked,
                    });
                }
            }
            summary.removal = Some(removal);
        }
        Ok(summary)
    }
}

/// What the two halves of a copy through the Mac have done so far, folded into
/// one picture for the progress bar.
#[derive(Default)]
struct Meter {
    files_total: usize,
    files_done: usize,
    bytes_total: u64,
    /// Bytes read from the phone, and bytes sent back to it.
    read: u64,
    sent: u64,
}

impl Meter {
    /// Takes in a report from one half and rewrites it as the whole's. Each
    /// byte of the selection crosses the cable twice, so progress is half of
    /// what has crossed: the speed and the time left then say how fast the
    /// selection is being copied.
    fn fold_in(&mut self, report: &mut TransferProgress) {
        match report.direction {
            Direction::Download => {
                self.files_total = report.files_total;
                self.files_done = report.files_done;
                self.bytes_total = report.bytes_total;
                self.read = report.bytes_done;
            }
            // (Nothing there is copied inside the Mac: the halves are the
            // phone's.)
            Direction::Upload | Direction::Local => self.sent = report.bytes_done,
        }
        report.files_total = self.files_total;
        report.files_done = self.files_done;
        report.bytes_total = self.bytes_total;
        report.bytes_done = (self.read + self.sent) / 2;
    }
}

/// Which selected items a move may take the originals of: every step of the
/// item was copied down and checked, and the copy that went up was read back
/// whole. Indexed like `items`. An item the upload does not account for stays.
fn items_that_may_go(
    items: &[String],
    origins: &[super::service::Origin],
    outcomes: &[Outcome],
    delivered: &[Delivered],
) -> Vec<bool> {
    let mut good: Vec<bool> = items
        .iter()
        .map(|item| {
            delivered
                .iter()
                .any(|sent| sent.name == *item && sent.confirmed)
        })
        .collect();
    for (origin, outcome) in origins.iter().zip(outcomes) {
        if *outcome != Outcome::Verified {
            good[origin.top] = false;
        }
    }
    good
}

/// Whether something in `item` was reported as failed, going down or up.
fn has_failure(item: &str, failures: &[crate::dto::FileFailure], delivered: &[Delivered]) -> bool {
    failures
        .iter()
        .any(|failure| failure.label == item || failure.label.starts_with(&format!("{item}/")))
        || delivered
            .iter()
            .any(|sent| sent.name == item && sent.failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::FileFailure;
    use crate::service::transfer::service::Origin;

    fn sent(name: &str, failed: bool, confirmed: bool) -> Delivered {
        Delivered {
            name: name.into(),
            failed,
            skipped: false,
            confirmed,
            node: std::sync::Arc::new(super::super::upload::LocalNode::for_test(name, false, 1)),
        }
    }

    fn origin(top: usize) -> Origin {
        Origin::for_test(top)
    }

    #[test]
    fn an_item_may_go_only_if_it_was_checked_going_down_and_read_back_going_up() {
        let items = [
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        let origins = [origin(0), origin(1), origin(1), origin(2), origin(3)];
        let outcomes = [
            Outcome::Verified,
            Outcome::Verified,
            Outcome::Unchecked, // b: one file could not be checked going down
            Outcome::Verified,
            Outcome::Verified,
        ];
        let delivered = [
            sent("a", false, true),
            sent("b", false, true),
            sent("c", false, false), // c: sent but not read back
                                     // d: the upload does not account for it
        ];

        let good = items_that_may_go(&items, &origins, &outcomes, &delivered);

        assert_eq!(good, [true, false, false, false]);
    }

    #[test]
    fn a_failure_is_found_by_the_item_it_is_in() {
        let failures = [FileFailure {
            label: "DCIM/Camera/a.jpg".into(),
            error: Error::ReadOnly,
        }];

        assert!(has_failure("DCIM", &failures, &[]));
        assert!(!has_failure("DCIM2", &failures, &[]), "not just a prefix");
        assert!(!has_failure("Music", &failures, &[]));
        assert!(has_failure("Music", &[], &[sent("Music", true, false)]));
        assert!(!has_failure("Music", &[], &[sent("Music", false, false)]));
    }
}
