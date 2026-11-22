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

pub fn app_folder(home: &std::path::Path) -> std::path::PathBuf {
    home.join("Library/Application Support/open-mtp-client")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn the_app_folder_is_in_application_support() {
        assert_eq!(
            super::app_folder(Path::new("/Users/me")),
            PathBuf::from("/Users/me/Library/Application Support/open-mtp-client")
        );
    }
}
