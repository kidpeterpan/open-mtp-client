//! The sidebar's places: the Mac's own folders and mounted disks, and the
//! folders pinned on either side (kept in one JSON file beside the settings).

use std::path::{Path, PathBuf};

use super::mapper::{self, PlacesFile};
use super::mounts::{self, Mount};
use crate::dto::{Error, MOST_MIRRORS, MOST_PINS, MOST_VOLUMES, Pin, Place, SavedMirror};

pub struct Service {
    pins_file: PathBuf,
    home: PathBuf,
    /// Where disks are mounted (`/Volumes`).
    volumes: PathBuf,
    /// What is mounted, and how full: the kernel's word in the app, a list
    /// in tests.
    mounts: fn() -> Vec<Mount>,
}

impl Service {
    pub fn new(pins_file: PathBuf, home: PathBuf, volumes: PathBuf) -> Self {
        Self {
            pins_file,
            home,
            volumes,
            mounts: mounts::mounted,
        }
    }

    /// The same, with `mounts` saying what is mounted (tests).
    #[cfg(test)]
    pub(super) fn with_mounts(mut self, mounts: fn() -> Vec<Mount>) -> Self {
        self.mounts = mounts;
        self
    }

    /// Home, Desktop, Downloads and Pictures (those that are there), then the
    /// mounted disks by name. Reads the disk: run it off the UI thread.
    pub fn places(&self) -> Vec<Place> {
        let mut places: Vec<Place> = [
            ("Home", self.home.clone()),
            ("Desktop", self.home.join("Desktop")),
            ("Downloads", self.home.join("Downloads")),
            ("Pictures", self.home.join("Pictures")),
        ]
        .into_iter()
        .filter(|(_, path)| path.is_dir())
        .map(|(label, path)| Place {
            label: label.to_owned(),
            path: path.to_string_lossy().into_owned(),
            volume: false,
            space: None,
        })
        .collect();
        places.extend(self.disks());
        places
    }

    /// The mounted disks. A link is followed only when it is the start-up
    /// disk's (a link to `/`); nothing else is looked through, so a network
    /// mount that has stopped answering can't hold the list up.
    fn disks(&self) -> Vec<Place> {
        let Ok(entries) = std::fs::read_dir(&self.volumes) else {
            return Vec::new();
        };
        let mut disks: Vec<Place> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    return None;
                }
                let kind = entry.file_type().ok()?;
                let path = if kind.is_symlink() {
                    (std::fs::read_link(entry.path()).ok()? == Path::new("/"))
                        .then(|| "/".to_owned())?
                } else if kind.is_dir() {
                    entry.path().to_string_lossy().into_owned()
                } else {
                    return None;
                };
                Some(Place {
                    label: name,
                    path,
                    volume: true,
                    space: None,
                })
            })
            .collect();
        disks.sort_by_key(|a| a.label.to_lowercase());
        disks.truncate(MOST_VOLUMES);
        // One call for all of them, answered from what macOS already knows.
        let mounted = (self.mounts)();
        for disk in &mut disks {
            disk.space = mounted
                .iter()
                .find(|mount| mount.path == disk.path)
                .map(|mount| mount.space);
        }
        disks
    }

    /// What the file holds; nothing when there is none or it can't be read.
    fn read(&self) -> Option<PlacesFile> {
        std::fs::read(&self.pins_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PlacesFile>(&bytes).ok())
    }

    /// The pinned folders; none when there is no file or it can't be read.
    pub fn pins(&self) -> Vec<Pin> {
        self.read()
            .map(|file| mapper::to_pins(&file))
            .unwrap_or_default()
    }

    /// The saved mirrors, kept in the same file.
    pub fn mirrors(&self) -> Vec<SavedMirror> {
        self.read()
            .map(|file| mapper::to_mirrors(&file))
            .unwrap_or_default()
    }

    /// Writes the pins, keeping the saved mirrors as the file has them.
    pub fn save_pins(&self, pins: &[Pin]) -> Result<(), Error> {
        self.write(pins, &self.mirrors())
    }

    /// Writes the saved mirrors, keeping the pins as the file has them.
    pub fn save_mirrors(&self, mirrors: &[SavedMirror]) -> Result<(), Error> {
        self.write(&self.pins(), mirrors)
    }

    /// Writes pins (at most `MOST_PINS`) and mirrors (at most
    /// `MOST_MIRRORS`) beside the file and renames it over, so a crash leaves
    /// the old lists rather than half of one.
    fn write(&self, pins: &[Pin], mirrors: &[SavedMirror]) -> Result<(), Error> {
        let fail =
            |err: std::io::Error| Error::General(format!("couldn't save the sidebar: {err}"));
        if let Some(folder) = self.pins_file.parent() {
            std::fs::create_dir_all(folder).map_err(fail)?;
        }
        let kept = &pins[..pins.len().min(MOST_PINS)];
        let mirrors = &mirrors[..mirrors.len().min(MOST_MIRRORS)];
        let json = serde_json::to_vec_pretty(&mapper::to_file(kept, mirrors))
            .map_err(|err| Error::General(format!("couldn't save the sidebar: {err}")))?;
        let temporary = self.pins_file.with_extension("json.tmp");
        std::fs::write(&temporary, json).map_err(fail)?;
        std::fs::rename(&temporary, &self.pins_file).map_err(fail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{PinPhone, PinSide};

    fn only_the_start_up_disk() -> Vec<Mount> {
        vec![Mount {
            path: "/".into(),
            space: crate::dto::DiskSpace {
                free: 312_000_000_000,
                capacity: 1_000_000_000_000,
            },
        }]
    }

    #[test]
    fn each_disk_says_how_full_it_is_when_macos_knows() {
        let m = mac();
        std::os::unix::fs::symlink("/", m.volumes.path().join("Macintosh HD")).unwrap();
        std::fs::create_dir(m.volumes.path().join("Backup")).unwrap();
        let service = m.service().with_mounts(only_the_start_up_disk);

        let places = service.places();

        let disk = |label: &str| places.iter().find(|place| place.label == label).unwrap();
        assert_eq!(
            disk("Macintosh HD").space,
            Some(crate::dto::DiskSpace {
                free: 312_000_000_000,
                capacity: 1_000_000_000_000
            })
        );
        assert_eq!(
            disk("Backup").space,
            None,
            "not mounted as far as macOS says"
        );
        assert!(
            places
                .iter()
                .filter(|place| !place.volume)
                .all(|place| place.space.is_none())
        );
    }

    struct Mac {
        home: tempfile::TempDir,
        volumes: tempfile::TempDir,
        support: tempfile::TempDir,
    }

    fn mac() -> Mac {
        Mac {
            home: tempfile::tempdir().unwrap(),
            volumes: tempfile::tempdir().unwrap(),
            support: tempfile::tempdir().unwrap(),
        }
    }

    impl Mac {
        fn service(&self) -> Service {
            Service::new(
                self.support.path().join("places.json"),
                self.home.path().to_path_buf(),
                self.volumes.path().to_path_buf(),
            )
        }
    }

    fn labels(places: &[Place]) -> Vec<&str> {
        places.iter().map(|p| p.label.as_str()).collect()
    }

    fn mac_pin(path: &str) -> Pin {
        Pin {
            side: PinSide::Mac,
            path: path.into(),
            label: "x".into(),
            phone: None,
        }
    }

    fn phone_pin(path: &str, serial: &str, storage_id: u64) -> Pin {
        Pin {
            side: PinSide::Phone,
            path: path.into(),
            label: "Camera".into(),
            phone: Some(PinPhone {
                serial: serial.into(),
                storage_id,
                storage_name: "Internal".into(),
            }),
        }
    }

    #[test]
    fn the_macs_own_folders_that_are_there_then_the_disks_by_name() {
        let m = mac();
        for folder in ["Desktop", "Downloads"] {
            std::fs::create_dir(m.home.path().join(folder)).unwrap();
        }
        std::fs::create_dir(m.volumes.path().join("backup")).unwrap();
        std::fs::create_dir(m.volumes.path().join("Card")).unwrap();
        std::fs::create_dir(m.volumes.path().join(".hidden")).unwrap();
        std::fs::write(m.volumes.path().join("a file"), b"").unwrap();
        std::os::unix::fs::symlink("/", m.volumes.path().join("Macintosh HD")).unwrap();
        std::os::unix::fs::symlink("/tmp", m.volumes.path().join("elsewhere")).unwrap();

        let places = m.service().places();

        assert_eq!(
            labels(&places),
            [
                "Home",
                "Desktop",
                "Downloads",
                "backup",
                "Card",
                "Macintosh HD"
            ]
        );
        let start_up = places.iter().find(|p| p.label == "Macintosh HD").unwrap();
        assert_eq!(start_up.path, "/");
        assert!(start_up.volume && !places[0].volume);
    }

    #[test]
    fn a_dozen_disks_at_most_and_none_without_a_volumes_folder() {
        let m = mac();
        for n in 0..20 {
            std::fs::create_dir(m.volumes.path().join(format!("Disk {n:02}"))).unwrap();
        }
        assert_eq!(
            m.service().places().iter().filter(|p| p.volume).count(),
            MOST_VOLUMES
        );

        let gone = Service::new(
            m.support.path().join("p.json"),
            m.home.path().to_path_buf(),
            m.volumes.path().join("nowhere"),
        );
        assert_eq!(labels(&gone.places()), ["Home"]);
    }

    #[test]
    fn pins_are_kept_and_read_back_on_either_side() {
        let m = mac();
        let pins = vec![
            mac_pin("/Users/me/Work"),
            phone_pin("/DCIM/Camera", "ABC123", 65537),
        ];

        m.service().save_pins(&pins).unwrap();

        assert_eq!(m.service().pins(), pins);
        assert!(!m.support.path().join("places.json.tmp").exists());
    }

    #[test]
    fn what_a_pins_file_holds_is_not_trusted() {
        let m = mac();
        assert!(m.service().pins().is_empty(), "no file yet");
        std::fs::write(
            m.support.path().join("places.json"),
            r#"{"pins": [
                {"side": "mac", "path": "relative/path", "label": "x"},
                {"side": "moon", "path": "/a", "label": "x"},
                {"side": "phone", "path": "/DCIM", "label": "x"},
                {"side": "phone", "path": "/DCIM", "label": "x", "serial": "", "storage_id": 1},
                {"side": "mac", "path": "/ok", "label": "ok"}
            ]}"#,
        )
        .unwrap();

        assert_eq!(
            m.service().pins(),
            [Pin {
                label: "ok".into(),
                ..mac_pin("/ok")
            }]
        );

        std::fs::write(m.support.path().join("places.json"), b"not json").unwrap();
        assert!(m.service().pins().is_empty());
        std::fs::write(m.support.path().join("places.json"), br#"{"version": 1}"#).unwrap();
        assert!(m.service().pins().is_empty(), "an older file without pins");
    }

    #[test]
    fn at_most_fifty_pins_are_kept() {
        let m = mac();
        let many: Vec<Pin> = (0..60).map(|n| mac_pin(&format!("/f{n}"))).collect();

        m.service().save_pins(&many).unwrap();

        assert_eq!(m.service().pins().len(), MOST_PINS);
    }

    #[test]
    fn the_same_place_is_the_same_folder_on_the_same_phone_and_storage() {
        let a = phone_pin("/DCIM", "ABC", 1);

        assert!(a.same_place(&Pin {
            label: "other".into(),
            ..a.clone()
        }));
        assert!(!a.same_place(&phone_pin("/DCIM", "XYZ", 1)));
        assert!(!a.same_place(&phone_pin("/DCIM", "ABC", 2)));
        assert!(!a.same_place(&mac_pin("/DCIM")));
        assert!(mac_pin("/a").same_place(&mac_pin("/a")));
    }

    fn mirror(phone_folder: &str, mac_folder: &str) -> SavedMirror {
        SavedMirror {
            phone: PinPhone {
                serial: "ABC123".into(),
                storage_id: 65537,
                storage_name: "Internal".into(),
            },
            phone_folder: phone_folder.into(),
            mac_folder: mac_folder.into(),
            direction: crate::dto::MirrorDirection::ToMac,
            remove_extras: true,
        }
    }

    #[test]
    fn saved_mirrors_and_pins_share_the_file_without_losing_each_other() {
        let m = mac();
        let service = m.service();
        assert!(service.mirrors().is_empty(), "no file yet");
        service.save_pins(&[mac_pin("/Users/me/Work")]).unwrap();

        let mirrors = vec![mirror("/DCIM/Camera", "/Users/me/Phone photos")];
        service.save_mirrors(&mirrors).unwrap();
        assert_eq!(service.mirrors(), mirrors);
        assert_eq!(service.pins().len(), 1, "the pins stay");

        service.save_pins(&[]).unwrap();
        assert_eq!(service.mirrors(), mirrors, "and the mirrors stay");

        let many: Vec<SavedMirror> = (0..30).map(|n| mirror(&format!("/f{n}"), "/m")).collect();
        service.save_mirrors(&many).unwrap();
        assert_eq!(service.mirrors().len(), MOST_MIRRORS);
        let written = std::fs::read_to_string(m.support.path().join("places.json")).unwrap();
        assert_eq!(
            written.matches("\"phone_folder\"").count(),
            MOST_MIRRORS,
            "no more are written than are kept"
        );
    }

    #[test]
    fn what_a_file_says_of_mirrors_is_not_trusted() {
        let m = mac();
        std::fs::write(
            m.support.path().join("places.json"),
            r#"{"mirrors": [
                {"serial": "A", "storage_id": 1, "phone_folder": "DCIM", "mac_folder": "/m", "direction": "to_mac"},
                {"serial": "A", "storage_id": 1, "phone_folder": "/DCIM", "mac_folder": "m", "direction": "to_mac"},
                {"serial": "", "storage_id": 1, "phone_folder": "/DCIM", "mac_folder": "/m", "direction": "to_mac"},
                {"serial": "A", "storage_id": 1, "phone_folder": "/DCIM", "mac_folder": "/m", "direction": "sideways"},
                {"serial": "A", "storage_id": 1, "phone_folder": "/Music", "mac_folder": "/Users/me/Music", "direction": "to_phone"}
            ]}"#,
        )
        .unwrap();

        let mirrors = m.service().mirrors();

        assert_eq!(mirrors.len(), 1);
        assert_eq!(mirrors[0].direction, crate::dto::MirrorDirection::ToPhone);
        assert!(!mirrors[0].remove_extras, "not said: kept off");
        assert!(mirrors[0].same_mirror(&mirrors[0].clone()));
        let other_way = SavedMirror {
            direction: crate::dto::MirrorDirection::ToMac,
            ..mirrors[0].clone()
        };
        assert!(!mirrors[0].same_mirror(&other_way));
        let removing = SavedMirror {
            remove_extras: true,
            ..mirrors[0].clone()
        };
        assert!(
            mirrors[0].same_mirror(&removing),
            "what it removes may differ"
        );
    }

    #[test]
    fn a_pins_file_that_cannot_be_written_says_so() {
        let m = mac();
        std::fs::write(m.support.path().join("blocked"), b"").unwrap();
        let service = Service::new(
            m.support.path().join("blocked/places.json"),
            m.home.path().to_path_buf(),
            m.volumes.path().to_path_buf(),
        );

        assert!(service.save_pins(&[mac_pin("/a")]).is_err());
    }
}
