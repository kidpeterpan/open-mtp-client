//! Client error → dto, shared by every domain service.

use crate::dto::Error;

/// `android_file_transfer_running` lets an exclusive-access failure name the
/// usual culprit on macOS instead of a generic "in use".
pub fn to_error(err: mtp_rs::Error, android_file_transfer_running: bool) -> Error {
    use mtp_rs::Error as Mtp;

    match err {
        Mtp::NoDevice => Error::NoDevice,
        Mtp::ExclusiveAccess | Mtp::Busy if android_file_transfer_running => {
            Error::AndroidFileTransferRunning
        }
        Mtp::ExclusiveAccess | Mtp::Busy => Error::DeviceInUse,
        Mtp::AccessDenied => Error::AllowStorageAccess,
        Mtp::PermissionDenied => Error::PermissionDenied,
        Mtp::Disconnected | Mtp::DeviceReset => Error::Disconnected,
        Mtp::Timeout => Error::Timeout,
        other => Error::General(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_access_blames_android_file_transfer_when_it_runs() {
        assert_eq!(
            to_error(mtp_rs::Error::ExclusiveAccess, true),
            Error::AndroidFileTransferRunning
        );
        assert_eq!(
            to_error(mtp_rs::Error::Busy, true),
            Error::AndroidFileTransferRunning
        );
    }

    #[test]
    fn exclusive_access_is_in_use_otherwise() {
        assert_eq!(
            to_error(mtp_rs::Error::ExclusiveAccess, false),
            Error::DeviceInUse
        );
        assert_eq!(to_error(mtp_rs::Error::Busy, false), Error::DeviceInUse);
    }

    #[test]
    fn device_states_map_to_their_user_facing_errors() {
        assert_eq!(to_error(mtp_rs::Error::NoDevice, false), Error::NoDevice);
        assert_eq!(
            to_error(mtp_rs::Error::AccessDenied, false),
            Error::AllowStorageAccess
        );
        assert_eq!(
            to_error(mtp_rs::Error::PermissionDenied, false),
            Error::PermissionDenied
        );
        assert_eq!(
            to_error(mtp_rs::Error::Disconnected, false),
            Error::Disconnected
        );
        assert_eq!(
            to_error(mtp_rs::Error::DeviceReset, false),
            Error::Disconnected
        );
        assert_eq!(to_error(mtp_rs::Error::Timeout, false), Error::Timeout);
    }

    #[test]
    fn anything_else_keeps_the_client_message() {
        assert_eq!(
            to_error(mtp_rs::Error::StorageFull, false),
            Error::General("storage full".into())
        );
    }
}
