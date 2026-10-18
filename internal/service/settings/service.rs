use std::path::{Path, PathBuf};

use super::mapper::{self, FoldersFile, SettingsFile};
use super::queue_file;
use crate::dto::{Error, LastFolders, SavedTransfer, Settings};

/// Keeps the app's preferences in one JSON file.
pub struct Service {
    path: PathBuf,
}

impl Service {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The saved settings, or the defaults when there is no file yet or it
    /// can't be read: a damaged settings file should never stop the app.
    pub fn load(&self) -> Settings {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SettingsFile>(&bytes).ok())
            .map_or_else(Settings::default, |file| mapper::to_settings(&file))
    }

    /// Moves a settings file saved under an earlier name of the app to this
    /// one, once. A file already here always wins, and a failed move leaves
    /// the old file in place (the app then starts with defaults).
    pub fn adopt(&self, earlier: &Path) {
        if self.path.exists() || !earlier.is_file() {
            return;
        }
        if let Some(folder) = self.path.parent()
            && std::fs::create_dir_all(folder).is_err()
        {
            return;
        }
        if std::fs::rename(earlier, &self.path).is_ok()
            && let Some(old_folder) = earlier.parent()
        {
            // Only succeeds when the folder is now empty.
            let _ = std::fs::remove_dir(old_folder);
        }
    }

    /// Writes beside the file and renames over it, so a crash mid-save
    /// leaves the previous settings rather than half a file.
    pub fn save(&self, settings: &Settings) -> Result<(), Error> {
        let json = serde_json::to_vec_pretty(&mapper::to_file(settings))
            .map_err(|err| Error::General(format!("couldn't save settings: {err}")))?;
        write_whole(&self.path, &json)
    }

    /// Where the panes were left: its own file beside the settings, as it
    /// changes with every folder opened.
    fn folders_path(&self) -> PathBuf {
        self.path.with_file_name("folders.json")
    }

    /// The folders the panes were left at; none when there is no file yet or
    /// it can't be read.
    pub fn load_folders(&self) -> LastFolders {
        std::fs::read(self.folders_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FoldersFile>(&bytes).ok())
            .map_or_else(LastFolders::default, |file| mapper::to_folders(&file))
    }

    pub fn save_folders(&self, folders: &LastFolders) -> Result<(), Error> {
        let json = serde_json::to_vec_pretty(&mapper::to_folders_file(folders))
            .map_err(|err| Error::General(format!("couldn't save settings: {err}")))?;
        write_whole(&self.folders_path(), &json)
    }

    fn transfers_path(&self) -> PathBuf {
        self.path.with_file_name("transfers.json")
    }

    /// The transfers not finished when the app last ended; none when there
    /// is no file, or it can't be read as one.
    pub fn load_transfers(&self) -> Vec<SavedTransfer> {
        std::fs::read(self.transfers_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .map_or_else(Vec::new, queue_file::to_transfers)
    }

    pub fn save_transfers(&self, saved: &[SavedTransfer]) -> Result<(), Error> {
        let json = serde_json::to_vec_pretty(&queue_file::to_transfers_file(saved))
            .map_err(|err| Error::General(format!("couldn't save settings: {err}")))?;
        write_whole(&self.transfers_path(), &json)
    }
}

/// Writes `bytes` beside `path` and renames over it.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let fail = |err: std::io::Error| Error::General(format!("couldn't save settings: {err}"));
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(fail)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(fail)?;
    std::fs::rename(&temporary, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unfinished_transfers_are_kept_in_a_file_of_their_own() {
        use crate::dto::{Paste, PasteRequest, SavedJob};
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(dir.path().join("settings.json"));
        assert!(service.load_transfers().is_empty(), "no file yet");

        let kept = vec![SavedTransfer {
            phone: None,
            job: SavedJob::OnMac(PasteRequest {
                mode: Paste::Move,
                from_storage: 0,
                from: "/Users/me".into(),
                names: vec!["big.mov".into()],
                to_storage: 0,
                to: "/Volumes/Backup".into(),
            }),
        }];
        service.save_transfers(&kept).unwrap();

        assert_eq!(service.load_transfers(), kept);
        assert!(dir.path().join("transfers.json").exists());
        assert!(
            !dir.path().join("settings.json").exists(),
            "the settings untouched"
        );
        std::fs::write(dir.path().join("transfers.json"), b"not json").unwrap();
        assert!(
            service.load_transfers().is_empty(),
            "damaged: nothing, never a failure"
        );
    }

    #[test]
    fn where_the_panes_were_is_kept_in_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(dir.path().join("settings.json"));
        assert_eq!(
            service.load_folders(),
            LastFolders::default(),
            "no file yet"
        );

        let mut folders = LastFolders {
            mac: Some("/Users/me/Desktop".into()),
            ..LastFolders::default()
        };
        folders.keep_phone("pixel", 7, "/DCIM");
        folders.keep_mac("/Users/me/Music");
        assert_eq!(folders.recent_mac.len(), 1);
        assert_eq!(folders.recent_phone.len(), 1);
        service.save_folders(&folders).unwrap();

        assert!(dir.path().join("folders.json").exists());
        assert!(
            !dir.path().join("settings.json").exists(),
            "the settings are not touched"
        );
        assert_eq!(service.load_folders(), folders);
        std::fs::write(dir.path().join("folders.json"), b"not json").unwrap();
        assert_eq!(
            service.load_folders(),
            LastFolders::default(),
            "a damaged file is no folders"
        );
    }

    use std::fs;

    use super::*;
    use crate::dto::ThemeChoice;

    fn service(dir: &tempfile::TempDir) -> Service {
        Service::new(dir.path().join("open-mtp-client").join("settings.json"))
    }

    #[test]
    fn with_no_file_the_defaults_are_used() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(service(&dir).load(), Settings::default());
    }

    #[test]
    fn saved_settings_load_back_and_create_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        let settings = Settings {
            theme: ThemeChoice::Light,
            status_bar: false,
            ..Settings::default()
        };

        service.save(&settings).unwrap();

        assert_eq!(service.load(), settings);
        assert!(
            !dir.path()
                .join("open-mtp-client/settings.json.tmp")
                .exists()
        );
    }

    #[test]
    fn a_damaged_file_falls_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        fs::create_dir_all(dir.path().join("open-mtp-client")).unwrap();
        fs::write(
            dir.path().join("open-mtp-client/settings.json"),
            b"{not json",
        )
        .unwrap();

        assert_eq!(service.load(), Settings::default());
    }

    #[test]
    fn missing_fields_take_their_defaults_and_unknown_ones_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        fs::create_dir_all(dir.path().join("open-mtp-client")).unwrap();
        fs::write(
            dir.path().join("open-mtp-client/settings.json"),
            br#"{"theme":"dark","fromTheFuture":42}"#,
        )
        .unwrap();

        let settings = service.load();

        assert_eq!(settings.theme, ThemeChoice::Dark);
        assert!(settings.status_bar);
        assert!(settings.folders_first);
    }

    #[test]
    fn the_file_is_readable_json() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);

        service.save(&Settings::default()).unwrap();

        let text = fs::read_to_string(dir.path().join("open-mtp-client/settings.json")).unwrap();
        assert!(text.contains(r#""theme": "auto""#));
        assert!(text.contains(r#""version": 1"#));
    }

    #[test]
    fn a_save_that_cannot_write_says_why() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be.
        fs::write(dir.path().join("open-mtp-client"), b"").unwrap();

        let result = service(&dir).save(&Settings::default());

        assert!(
            matches!(result, Err(Error::General(m)) if m.starts_with("couldn't save settings"))
        );
    }
    #[test]
    fn adopt_moves_settings_saved_under_the_old_name() {
        let dir = tempfile::tempdir().unwrap();
        let old = Service::new(dir.path().join("mtprust").join("settings.json"));
        let changed = Settings {
            theme: ThemeChoice::Dark,
            ..Settings::default()
        };
        old.save(&changed).unwrap();

        let new = service(&dir);
        new.adopt(&dir.path().join("mtprust/settings.json"));

        assert_eq!(new.load(), changed);
        assert!(
            !dir.path().join("mtprust").exists(),
            "empty old folder removed"
        );
    }

    #[test]
    fn adopt_never_replaces_settings_already_saved_under_the_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let new = service(&dir);
        new.save(&Settings::default()).unwrap();
        let old = Service::new(dir.path().join("mtprust").join("settings.json"));
        old.save(&Settings {
            theme: ThemeChoice::Dark,
            ..Settings::default()
        })
        .unwrap();

        new.adopt(&dir.path().join("mtprust/settings.json"));

        assert_eq!(new.load(), Settings::default());
        assert!(dir.path().join("mtprust/settings.json").exists());
    }

    #[test]
    fn adopt_keeps_the_old_folder_when_something_else_is_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let old = Service::new(dir.path().join("mtprust").join("settings.json"));
        old.save(&Settings::default()).unwrap();
        fs::write(dir.path().join("mtprust/notes.txt"), b"mine").unwrap();

        service(&dir).adopt(&dir.path().join("mtprust/settings.json"));

        assert!(dir.path().join("mtprust/notes.txt").exists());
        assert!(!dir.path().join("mtprust/settings.json").exists());
    }

    #[test]
    fn adopt_does_nothing_without_an_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let new = service(&dir);

        new.adopt(&dir.path().join("mtprust/settings.json"));

        assert!(!new.path.exists());
        assert_eq!(new.load(), Settings::default());
    }

    #[test]
    fn adopt_leaves_the_old_file_when_the_new_folder_cannot_be_made() {
        let dir = tempfile::tempdir().unwrap();
        let old = Service::new(dir.path().join("mtprust").join("settings.json"));
        old.save(&Settings::default()).unwrap();
        fs::write(dir.path().join("blocked"), b"").unwrap();
        let new = Service::new(dir.path().join("blocked").join("settings.json"));

        new.adopt(&dir.path().join("mtprust/settings.json"));

        assert!(dir.path().join("mtprust/settings.json").exists());
    }
}
