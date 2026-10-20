//! What's new in each release, bundled with the app, newest first. Shown once
//! after an update (every release since the one last run, so a skipped
//! version is not missed) and always in Help's "What's new" tab.
//!
//! **A new version needs its notes here**: `every_version_of_the_app_has_its_notes`
//! fails when `Cargo.toml`'s version has no entry.

use internal::dto::Version;

use crate::i18n::key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    pub version: &'static str,
    pub notes: &'static [&'static str],
}

/// Every release, newest first.
pub const RELEASES: &[Release] = &[Release {
    version: "0.1.0",
    notes: &[
        key("whatsnew.0_1_0.1"),
        key("whatsnew.0_1_0.2"),
        key("whatsnew.0_1_0.3"),
        key("whatsnew.0_1_0.4"),
        key("whatsnew.0_1_0.5"),
        key("whatsnew.0_1_0.6"),
        key("whatsnew.0_1_0.7"),
        key("whatsnew.0_1_0.8"),
        key("whatsnew.0_1_0.9"),
        key("whatsnew.0_1_0.10"),
        key("whatsnew.0_1_0.11"),
        key("whatsnew.0_1_0.12"),
        key("whatsnew.0_1_0.13"),
        key("whatsnew.0_1_0.14"),
        key("whatsnew.0_1_0.15"),
        key("whatsnew.0_1_0.16"),
        key("whatsnew.0_1_0.17"),
        key("whatsnew.0_1_0.18"),
        key("whatsnew.0_1_0.19"),
        key("whatsnew.0_1_0.20"),
        key("whatsnew.0_1_0.21"),
        key("whatsnew.0_1_0.22"),
        key("whatsnew.0_1_0.23"),
        key("whatsnew.0_1_0.24"),
        key("whatsnew.0_1_0.25"),
        key("whatsnew.0_1_0.26"),
        key("whatsnew.0_1_0.27"),
        key("whatsnew.0_1_0.28"),
        key("whatsnew.0_1_0.29"),
        key("whatsnew.0_1_0.30"),
        key("whatsnew.0_1_0.31"),
        key("whatsnew.0_1_0.32"),
        key("whatsnew.0_1_0.33"),
        key("whatsnew.0_1_0.34"),
        key("whatsnew.0_1_0.35"),
        key("whatsnew.0_1_0.36"),
        key("whatsnew.0_1_0.37"),
        key("whatsnew.0_1_0.38"),
        key("whatsnew.0_1_0.39"),
        key("whatsnew.0_1_0.40"),
        key("whatsnew.0_1_0.41"),
        key("whatsnew.0_1_0.42"),
        key("whatsnew.0_1_0.43"),
        key("whatsnew.0_1_0.44"),
        key("whatsnew.0_1_0.45"),
        key("whatsnew.0_1_0.46"),
        key("whatsnew.0_1_0.47"),
        key("whatsnew.0_1_0.48"),
        key("whatsnew.0_1_0.49"),
        key("whatsnew.0_1_0.50"),
        key("whatsnew.0_1_0.51"),
        key("whatsnew.0_1_0.52"),
        key("whatsnew.0_1_0.53"),
        key("whatsnew.0_1_0.54"),
        key("whatsnew.0_1_0.55"),
        key("whatsnew.0_1_0.56"),
        key("whatsnew.0_1_0.57"),
        key("whatsnew.0_1_0.58"),
        key("whatsnew.0_1_0.59"),
        key("whatsnew.0_1_0.60"),
        key("whatsnew.0_1_0.61"),
        key("whatsnew.0_1_0.62"),
        key("whatsnew.0_1_0.63"),
        key("whatsnew.0_1_0.64"),
        key("whatsnew.0_1_0.65"),
        key("whatsnew.0_1_0.66"),
        key("whatsnew.0_1_0.67"),
    ],
}];

/// The version this app is.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml's version is major.minor.patch")
}

/// The releases to show when version `current` starts after `last_seen` ran:
/// the ones newer than `last_seen`, up to `current`. Nothing on a first run
/// (there is no "new" yet), and nothing when the version is the same or
/// older.
pub fn after_update(last_seen: Option<Version>, current: Version) -> Vec<Release> {
    releases_between(RELEASES, last_seen, current)
}

/// `after_update` over the list `releases`.
fn releases_between(
    releases: &[Release],
    last_seen: Option<Version>,
    current: Version,
) -> Vec<Release> {
    let Some(last_seen) = last_seen else {
        return Vec::new();
    };
    releases
        .iter()
        .filter(|release| {
            Version::parse(release.version)
                .is_some_and(|version| version > last_seen && version <= current)
        })
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use internal::dto::Language;

    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn notes_key(version: &str, number: usize) -> String {
        format!("whatsnew.{}.{number}", version.replace('.', "_"))
    }

    #[test]
    fn every_release_has_its_notes_in_all_four_languages() {
        let catalogs = crate::i18n::embedded();
        let mut missing = Vec::new();

        for release in RELEASES {
            for number in 1..=release.notes.len() {
                let key = notes_key(release.version, number);
                for language in Language::ALL {
                    if !catalogs.has_own(language, &key) {
                        missing.push(format!("{language:?} {key}"));
                    }
                }
            }
        }

        assert!(
            missing.is_empty(),
            "notes are written once per language in the catalog: {missing:#?}"
        );
    }

    #[test]
    fn every_version_of_the_app_has_its_notes() {
        assert_eq!(
            RELEASES[0].version,
            env!("CARGO_PKG_VERSION"),
            "the newest notes are this version's: write them in whats_new.rs"
        );
        let versions: Vec<Version> = RELEASES.iter().map(|release| v(release.version)).collect();
        assert!(
            versions.windows(2).all(|pair| pair[0] > pair[1]),
            "newest first, each once: {versions:?}"
        );
        assert!(RELEASES.iter().all(|release| !release.notes.is_empty()));
    }

    #[test]
    fn after_an_update_every_release_since_the_last_run_is_shown() {
        // Releases as they might be later on.
        let shown = |last: Option<&str>, now: &str| -> Vec<&str> {
            let releases = [
                Release {
                    version: "0.3.0",
                    notes: &["c"],
                },
                Release {
                    version: "0.2.0",
                    notes: &["b"],
                },
                Release {
                    version: "0.1.0",
                    notes: &["a"],
                },
            ];
            let last = last.map(v);
            releases
                .iter()
                .filter(|release| releases_between(&releases, last, v(now)).contains(release))
                .map(|release| release.version)
                .collect()
        };

        assert_eq!(
            shown(Some("0.1.0"), "0.3.0"),
            ["0.3.0", "0.2.0"],
            "a skipped version too"
        );
        assert_eq!(shown(Some("0.2.0"), "0.3.0"), ["0.3.0"]);
        assert_eq!(
            shown(Some("0.1.0"), "0.2.0"),
            ["0.2.0"],
            "notes written ahead of this version are not shown yet"
        );
        assert!(
            shown(None, "0.3.0").is_empty(),
            "a first run has nothing new"
        );
        assert!(
            shown(Some("0.3.0"), "0.3.0").is_empty(),
            "the same version again"
        );
        assert!(shown(Some("0.3.0"), "0.2.0").is_empty(), "an older version");
    }

    #[test]
    fn this_releases_notes_show_after_an_update_from_an_older_one() {
        let shown = after_update(Some(v("0.0.9")), current());
        assert_eq!(
            shown.first().map(|release| release.version),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(after_update(Some(current()), current()).is_empty());
        assert!(after_update(None, current()).is_empty());
    }
}
