//! A transfer's report, saved where its files went: one new `.tsv` file,
//! "Transfer report 2026-10-17 14.32.tsv", numbered when that name is taken
//! and never written over anything.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;

use super::Service;
use crate::dto::Error;
use crate::service::local;

/// The most numbered names tried before giving up.
const NAMES_TRIED: usize = 100;

/// The report's file name for `at`, the `n`th tried.
fn report_name(at: NaiveDateTime, n: usize) -> String {
    let stamp = at.format("%Y-%m-%d %H.%M");
    match n {
        0 => format!("Transfer report {stamp}.tsv"),
        n => format!("Transfer report {stamp} {}.tsv", n + 1),
    }
}

/// Writes `text` into `folder` as a new file named for `at`; says where.
pub(super) fn write_report(folder: &Path, text: &str, at: NaiveDateTime) -> Result<PathBuf, Error> {
    let shown = folder.to_string_lossy();
    for n in 0..NAMES_TRIED {
        let path = folder.join(report_name(at, n));
        // `create_new` fails when the name is there, so nothing is replaced.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                return file
                    .write_all(text.as_bytes())
                    .and_then(|()| file.flush())
                    .map(|()| path)
                    .map_err(|err| local::mapper::to_error(&err, &shown));
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(local::mapper::to_error(&err, &shown)),
        }
    }
    Err(Error::NameTaken(report_name(at, 0)))
}

impl Service {
    /// Saves a transfer's report into `folder` (the Mac folder it copied
    /// into), named for now; says where it went.
    pub async fn save_report(&self, folder: &str, text: &str) -> Result<String, Error> {
        let (folder, text) = (PathBuf::from(folder), text.to_owned());
        let now = chrono::Local::now().naive_local();
        tokio::task::spawn_blocking(move || write_report(&folder, &text, now))
            .await
            .map_err(|err| Error::General(err.to_string()))?
            .map(|path| path.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 17)
            .unwrap()
            .and_hms_opt(14, 32, 5)
            .unwrap()
    }

    #[test]
    fn a_report_is_a_new_file_named_for_the_minute_and_numbered_when_taken() {
        let dir = tempfile::tempdir().unwrap();

        let first = write_report(dir.path(), "File\ta.jpg\n", at()).unwrap();
        let second = write_report(dir.path(), "second", at()).unwrap();

        assert_eq!(
            first.file_name().unwrap(),
            "Transfer report 2026-10-17 14.32.tsv"
        );
        assert_eq!(
            second.file_name().unwrap(),
            "Transfer report 2026-10-17 14.32 2.tsv"
        );
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "File\ta.jpg\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second");
    }

    #[test]
    fn nothing_is_written_over_and_a_folder_that_cant_take_it_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..NAMES_TRIED {
            std::fs::write(dir.path().join(report_name(at(), n)), "theirs").unwrap();
        }
        assert!(matches!(
            write_report(dir.path(), "mine", at()),
            Err(Error::NameTaken(_))
        ));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(report_name(at(), 0))).unwrap(),
            "theirs"
        );

        let gone = dir.path().join("gone");
        assert!(write_report(&gone, "x", at()).is_err());
    }

    #[tokio::test]
    async fn the_service_saves_it_in_the_folder_given() {
        let dir = tempfile::tempdir().unwrap();
        let phone = tempfile::tempdir().unwrap();
        let device = std::sync::Arc::new(crate::service::device::Service::new(
            crate::service::device::Source::Virtual(crate::service::testing::virtual_phone(
                phone.path(),
            )),
        ));
        let explorer = std::sync::Arc::new(crate::service::explorer::Service::new(
            std::sync::Arc::clone(&device),
        ));
        let service = Service::new(device, explorer);

        let saved = service
            .save_report(&dir.path().to_string_lossy(), "x")
            .await
            .unwrap();

        assert!(saved.starts_with(&*dir.path().to_string_lossy()));
        assert!(saved.ends_with(".tsv"));
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "x");
    }
}
