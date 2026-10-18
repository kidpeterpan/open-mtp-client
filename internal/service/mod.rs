mod clock;
pub mod crash;
pub mod device;
pub mod doctor;
pub mod explorer;
pub(crate) mod failures;
pub mod help;
pub mod local;
mod mapper;
mod names;
pub mod places;
pub(crate) mod quarantine;
pub mod renaming;
pub mod settings;
pub mod thumbnail;
pub mod transfer;

#[cfg(test)]
pub(crate) mod testing;
