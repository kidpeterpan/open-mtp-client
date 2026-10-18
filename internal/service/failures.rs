//! How a batch of changes on the phone sorts its failures: those about one
//! item, after which the batch can carry on, and those that would only repeat
//! for every item after them. Shared by copying files and by copying and moving
//! them within the phone.

use crate::dto::{Error, FileFailure};

/// The error to report when a batch copied nothing and every file failed for
/// the same reason: that is a problem with the whole batch (an unwritable
/// folder, a phone that refuses everything) or a one-file copy, and it reads
/// better as the error itself than as a list of identical failures. Anything
/// that copied something, or failed for different reasons, is a list.
pub(crate) fn whole_batch_failure(
    copied: usize,
    already_there: usize,
    failures: &[FileFailure],
) -> Option<Error> {
    let first = failures.first()?;
    (copied == 0 && already_there == 0 && failures.iter().all(|f| f.error == first.error))
        .then(|| first.error.clone())
}

/// Phone errors that are about one object, after which the phone is still
/// talking: it answered the request, it just wouldn't (or couldn't) do that
/// one. `Other` is a response code the library has no name for, such as
/// `GeneralError`, which is how phones refuse a file name they dislike (a `?`
/// on an SD card, say); the library's own rule is that an answered request
/// leaves the session usable. Everything else (no phone, a held or reset
/// session, a timeout, a full phone, an unknown I/O error) is about the
/// connection. A phone that wrongly answered everything this way would fail
/// every file quickly and be reported as the one shared error.
pub(crate) fn is_about_one_object(err: &mtp_rs::Error) -> bool {
    use mtp_rs::Error as Phone;
    matches!(
        err,
        Phone::NotFound
            | Phone::StaleHandle
            | Phone::AccessDenied
            | Phone::Unsupported
            | Phone::Busy
            | Phone::InvalidData { .. }
            | Phone::Other { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answered_requests_are_about_one_object() {
        use mtp_rs::Error as Phone;
        for err in [
            Phone::NotFound,
            Phone::StaleHandle,
            Phone::AccessDenied,
            Phone::Unsupported,
            Phone::Busy,
            Phone::InvalidData {
                message: "x".into(),
            },
            Phone::Other {
                detail: "GeneralError".into(),
            },
        ] {
            assert!(is_about_one_object(&err), "{err:?}");
        }
    }

    #[test]
    fn a_lost_slow_full_or_held_phone_is_about_the_connection() {
        use mtp_rs::Error as Phone;
        for err in [
            Phone::Disconnected,
            Phone::DeviceReset,
            Phone::Timeout,
            Phone::NoDevice,
            Phone::ExclusiveAccess,
            Phone::PermissionDenied,
            Phone::StorageFull,
            Phone::Cancelled,
            Phone::Io {
                message: "x".into(),
            },
        ] {
            assert!(!is_about_one_object(&err), "{err:?}");
        }
    }

    fn failed(label: &str, error: Error) -> FileFailure {
        FileFailure {
            label: label.into(),
            error,
        }
    }

    #[test]
    fn nothing_done_for_one_reason_is_the_batchs_own_error() {
        let same = [failed("a", Error::ReadOnly), failed("b", Error::ReadOnly)];

        assert_eq!(whole_batch_failure(0, 0, &same), Some(Error::ReadOnly));
        assert_eq!(whole_batch_failure(0, 0, &same[..1]), Some(Error::ReadOnly));
    }

    #[test]
    fn mixed_reasons_or_some_success_stay_a_list() {
        let mixed = [failed("a", Error::ReadOnly), failed("b", Error::Timeout)];
        let same = [failed("a", Error::ReadOnly)];

        assert_eq!(whole_batch_failure(0, 0, &mixed), None);
        assert_eq!(whole_batch_failure(1, 0, &same), None, "something was done");
        assert_eq!(
            whole_batch_failure(0, 1, &same),
            None,
            "something was there"
        );
        assert_eq!(whole_batch_failure(0, 0, &[]), None);
    }
}
