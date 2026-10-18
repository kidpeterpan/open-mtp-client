//! One module per domain. Each `handler.rs` declares the `Service` trait it
//! needs; `wire.rs` binds the concrete `internal::service` types to them.

pub mod crash;
pub mod device;
pub mod diagnosis;
pub mod drop_spot;
pub mod error;
pub mod explorer;
pub mod format;
pub mod help;
pub mod import;
pub mod info;
pub mod menubar;
pub mod mirror;
pub mod places;
pub mod preview;
pub mod search;
pub mod settings;
pub mod space;
pub mod transfer;

#[cfg(test)]
pub(crate) mod testing;
