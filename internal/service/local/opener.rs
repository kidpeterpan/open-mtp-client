//! Opening a file in its Mac app, as a double-click in Finder does (`open`),
//! and the part of System Settings that lets the app into a private folder.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::dto::{Error, PrivacyPane};

/// Opens files in their apps. A seam: the app runs `open`, tests record.
pub trait Opener: Send + Sync {
    fn open(&self, path: &Path) -> std::io::Result<()>;
    /// Opens System Settings on `pane`.
    fn open_privacy_settings(&self, pane: PrivacyPane) -> std::io::Result<()>;
}

/// System Settings' address for `pane`: one of two fixed addresses, never
/// anything built from a path.
pub fn privacy_settings_url(pane: PrivacyPane) -> &'static str {
    match pane {
        PrivacyPane::FilesAndFolders => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders"
        }
        PrivacyPane::FullDiskAccess => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        }
    }
}

/// `opener.open(path)`, with what went wrong said as the app says it.
pub fn open_in_app(opener: &dyn Opener, path: &Path) -> Result<(), Error> {
    opener
        .open(path)
        .map_err(|err| super::mapper::to_error(&err, &path.to_string_lossy()))
}

/// The real one: `open`, which hands the file to Launch Services and returns.
#[derive(Default)]
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&self, path: &Path) -> std::io::Result<()> {
        run_open(&[std::ffi::OsStr::new("--"), path.as_os_str()])
    }

    fn open_privacy_settings(&self, pane: PrivacyPane) -> std::io::Result<()> {
        run_open(&[std::ffi::OsStr::new(privacy_settings_url(pane))])
    }
}

/// Runs `open` with `args` and lets it go.
fn run_open(args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
    let mut child = Command::new("open")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reaped when it is done, off the UI thread.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// What tests see: every file opened, and every settings pane.
#[derive(Default)]
pub struct RecordingOpener {
    pub opened: Mutex<Vec<std::path::PathBuf>>,
    pub settings: Mutex<Vec<PrivacyPane>>,
}

impl Opener for RecordingOpener {
    fn open(&self, path: &Path) -> std::io::Result<()> {
        self.opened.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }

    fn open_privacy_settings(&self, pane: PrivacyPane) -> std::io::Result<()> {
        self.settings.lock().unwrap().push(pane);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Refusing;

    impl Opener for Refusing {
        fn open(&self, _path: &Path) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }

        fn open_privacy_settings(&self, _pane: PrivacyPane) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }
    }

    #[test]
    fn each_pane_is_one_fixed_system_settings_address() {
        assert_eq!(
            privacy_settings_url(PrivacyPane::FilesAndFolders),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders"
        );
        assert_eq!(
            privacy_settings_url(PrivacyPane::FullDiskAccess),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        );
        let recorder = RecordingOpener::default();
        recorder
            .open_privacy_settings(PrivacyPane::FullDiskAccess)
            .unwrap();
        assert_eq!(
            *recorder.settings.lock().unwrap(),
            [PrivacyPane::FullDiskAccess]
        );
        assert!(
            Refusing
                .open_privacy_settings(PrivacyPane::FilesAndFolders)
                .is_err()
        );
    }

    #[test]
    fn files_are_opened_and_a_refusal_is_said_in_the_apps_words() {
        let recorder = RecordingOpener::default();

        open_in_app(&recorder, Path::new("/tmp/a.pdf")).unwrap();

        assert_eq!(*recorder.opened.lock().unwrap(), [Path::new("/tmp/a.pdf")]);
        assert!(open_in_app(&Refusing, Path::new("/tmp/a.pdf")).is_err());
    }
}
