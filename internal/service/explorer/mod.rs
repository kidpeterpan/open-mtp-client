mod bounded;
mod listing;
mod mapper;
mod memory;
mod natural;
mod paste;
#[cfg(test)]
mod pasting;
mod raw;
#[cfg(test)]
mod remembering;
mod search;
#[cfg(test)]
mod searching;
mod service;
#[cfg(test)]
mod silent;
#[cfg(test)]
mod switching;
mod tally;

pub(crate) use bounded::{Limits, read_children};

/// `path` as the explorer writes a phone path: absolute, no trailing slash.
pub(crate) fn normalize_path(path: &str) -> String {
    service::normalize(path)
}

/// The date the pane shows for `object` (its own, or the Mac's local time for
/// one that carries an offset), so whatever is filed by date agrees with it.
pub(crate) fn shown_date(object: &mtp_rs::mtp::ObjectInfo) -> Option<crate::dto::Timestamp> {
    mapper::to_entry(object, crate::service::clock::local_offset_minutes).modified
}
pub use listing::Listing;
pub use search::Walk;
pub(crate) use service::{Folder, delete_object};
pub use service::{Service, sort};
