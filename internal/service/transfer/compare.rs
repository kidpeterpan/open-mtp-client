//! What a mirror has to do: the source folder's contents compared with the
//! target's, by path relative to each folder.
//!
//! Paths are matched ignoring case, as APFS does (and so a phone's `IMG.JPG`
//! is the Mac's `img.jpg`). Whatever can't be mirrored without one thing
//! overwriting another is a conflict, left alone on both sides and reported:
//! two items on one side whose names differ only in case (Android's own
//! storage tells case apart; the Mac doesn't), and a file where a folder is
//! (or the other way round). Hidden items (any part of the path starting
//! with a dot) are left alone on both sides: never copied, never counted as
//! extra, never removed. That covers the app's own part files and scratch
//! folders, and the Mac's `.DS_Store` and `._*`.

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use super::mapper;
use super::service::size_is_known;

/// One file or folder of a tree being mirrored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    /// Relative to the folder being mirrored, `/`-separated: "Camera/a.jpg".
    pub(super) path: String,
    pub(super) is_folder: bool,
    pub(super) size: u64,
    pub(super) modified: Option<SystemTime>,
}

/// Why something is left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Clash {
    /// Another source item has the same name but for case.
    CaseTwin,
    /// A file on one side is a folder on the other.
    FileAndFolder,
}

/// One thing a mirror does, or would do, in order: a folder comes before
/// what goes in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    MakeFolder {
        path: String,
    },
    CopyNew {
        path: String,
    },
    /// The target's file of this path differs; `target` is its own path,
    /// which may differ from the source's in case.
    Replace {
        path: String,
        target: String,
    },
    /// Only in the target. Removed only when asked (`remove`).
    Extra {
        target: String,
        is_folder: bool,
        remove: bool,
    },
    Conflict {
        path: String,
        why: Clash,
    },
}

/// Left alone on both sides: anything hidden, at any depth.
fn left_alone(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

/// The paths above `path`, nearest first: "a/b/c" gives "a/b", "a".
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices()
        .rev()
        .filter(|(_, c)| *c == '/')
        .map(|(at, _)| &path[..at])
}

/// The same file: the same size (or a size the phone doesn't know, on either
/// side) and the
/// same date within two seconds, as a download judges "already there". With
/// no date on either side, the size alone decides.
pub(super) fn is_same(source: &Item, target: &Item) -> bool {
    let sizes =
        !size_is_known(source.size) || !size_is_known(target.size) || source.size == target.size;
    let dates = match (source.modified, target.modified) {
        (Some(a), Some(b)) => mapper::same_moment(a, b),
        _ => true,
    };
    sizes && dates
}

/// What makes `target` a mirror of `source`.
pub(super) fn compare(source: &[Item], target: &[Item], remove_extras: bool) -> Vec<Action> {
    let source: Vec<&Item> = source.iter().filter(|i| !left_alone(&i.path)).collect();
    let target: Vec<&Item> = target.iter().filter(|i| !left_alone(&i.path)).collect();

    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in &source {
        *counts.entry(key(&item.path)).or_default() += 1;
    }
    let mut targets: HashMap<String, &Item> = HashMap::new();
    let mut target_counts: HashMap<String, usize> = HashMap::new();
    for item in &target {
        let k = key(&item.path);
        *target_counts.entry(k.clone()).or_default() += 1;
        targets.insert(k, *item);
    }

    let mut actions = Vec::new();
    // Source folders nothing is done below (a conflict), by key.
    let mut stopped: HashSet<String> = HashSet::new();
    let mut sorted = source.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    for item in sorted {
        let k = key(&item.path);
        if ancestors(&k).any(|above| stopped.contains(above)) {
            continue;
        }
        let twins = counts.get(&k).copied().unwrap_or(0) > 1
            || target_counts.get(&k).copied().unwrap_or(0) > 1;
        if twins {
            stopped.insert(k);
            actions.push(Action::Conflict {
                path: item.path.clone(),
                why: Clash::CaseTwin,
            });
            continue;
        }
        match targets.get(&k) {
            None if item.is_folder => actions.push(Action::MakeFolder {
                path: item.path.clone(),
            }),
            None => actions.push(Action::CopyNew {
                path: item.path.clone(),
            }),
            Some(there) if there.is_folder != item.is_folder => {
                stopped.insert(k);
                actions.push(Action::Conflict {
                    path: item.path.clone(),
                    why: Clash::FileAndFolder,
                });
            }
            Some(_) if item.is_folder => {}
            Some(there) if is_same(item, there) => {}
            Some(there) => actions.push(Action::Replace {
                path: item.path.clone(),
                target: there.path.clone(),
            }),
        }
    }

    // Only in the target: the topmost of each such branch, and nothing
    // below a conflict.
    let mut extras: Vec<&Item> = target
        .iter()
        .copied()
        .filter(|i| !counts.contains_key(&key(&i.path)))
        .collect();
    extras.sort_by(|a, b| a.path.cmp(&b.path));
    let mut extra_folders: HashSet<String> = HashSet::new();
    for item in extras {
        let k = key(&item.path);
        if ancestors(&k).any(|above| extra_folders.contains(above) || stopped.contains(above)) {
            continue;
        }
        if item.is_folder {
            extra_folders.insert(k);
        }
        actions.push(Action::Extra {
            target: item.path.clone(),
            is_folder: item.is_folder,
            remove: remove_extras,
        });
    }
    actions
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn at(secs: u64) -> Option<SystemTime> {
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    }

    fn file(path: &str, size: u64, when: u64) -> Item {
        Item {
            path: path.into(),
            is_folder: false,
            size,
            modified: at(when),
        }
    }

    fn folder(path: &str) -> Item {
        Item {
            path: path.into(),
            is_folder: true,
            size: 0,
            modified: None,
        }
    }

    #[test]
    fn new_changed_and_the_same_are_told_apart() {
        let source = [
            folder("Camera"),
            file("Camera/new.jpg", 5, 100),
            file("Camera/same.jpg", 5, 100),
            file("Camera/bigger.jpg", 6, 100),
            file("Camera/later.jpg", 5, 900),
            file("Camera/close.jpg", 5, 102),
            folder("Fresh"),
            file("Fresh/a.jpg", 1, 1),
        ];
        let target = [
            folder("Camera"),
            file("Camera/same.jpg", 5, 100),
            file("Camera/bigger.jpg", 5, 100),
            file("Camera/later.jpg", 5, 100),
            file("Camera/close.jpg", 5, 100),
        ];

        assert_eq!(
            compare(&source, &target, false),
            [
                Action::Replace {
                    path: "Camera/bigger.jpg".into(),
                    target: "Camera/bigger.jpg".into()
                },
                Action::Replace {
                    path: "Camera/later.jpg".into(),
                    target: "Camera/later.jpg".into()
                },
                Action::CopyNew {
                    path: "Camera/new.jpg".into()
                },
                Action::MakeFolder {
                    path: "Fresh".into()
                },
                Action::CopyNew {
                    path: "Fresh/a.jpg".into()
                },
            ]
        );
    }

    #[test]
    fn what_is_only_in_the_target_is_kept_unless_asked_and_counted_once_per_branch() {
        let source = [file("a.jpg", 1, 1)];
        let target = [
            file("a.jpg", 1, 1),
            file("old.jpg", 1, 1),
            folder("Gone"),
            folder("Gone/Deeper"),
            file("Gone/Deeper/x.jpg", 1, 1),
        ];

        let kept = compare(&source, &target, false);
        let removed = compare(&source, &target, true);

        assert_eq!(
            kept,
            [
                Action::Extra {
                    target: "Gone".into(),
                    is_folder: true,
                    remove: false
                },
                Action::Extra {
                    target: "old.jpg".into(),
                    is_folder: false,
                    remove: false
                },
            ]
        );
        assert!(
            removed
                .iter()
                .all(|a| matches!(a, Action::Extra { remove: true, .. }))
        );
        assert_eq!(removed.len(), 2);
    }

    #[test]
    fn names_match_ignoring_case_as_the_mac_does() {
        let source = [
            file("IMG.JPG", 2, 1),
            folder("DCIM"),
            file("DCIM/b.jpg", 1, 1),
        ];
        let target = [
            file("img.jpg", 1, 1),
            folder("dcim"),
            file("dcim/b.jpg", 1, 1),
        ];

        assert_eq!(
            compare(&source, &target, true),
            [Action::Replace {
                path: "IMG.JPG".into(),
                target: "img.jpg".into()
            }],
            "nothing new and nothing extra: dcim is DCIM"
        );
    }

    #[test]
    fn two_source_names_that_differ_only_in_case_are_left_alone_with_all_below() {
        let source = [
            file("a.jpg", 1, 1),
            file("A.jpg", 2, 1),
            folder("X"),
            file("X/in.jpg", 1, 1),
            folder("x"),
        ];
        let target = [file("a.jpg", 9, 9), folder("x"), file("x/old.jpg", 1, 1)];

        let actions = compare(&source, &target, true);

        assert_eq!(
            actions,
            [
                Action::Conflict {
                    path: "A.jpg".into(),
                    why: Clash::CaseTwin
                },
                Action::Conflict {
                    path: "X".into(),
                    why: Clash::CaseTwin
                },
                Action::Conflict {
                    path: "a.jpg".into(),
                    why: Clash::CaseTwin
                },
                Action::Conflict {
                    path: "x".into(),
                    why: Clash::CaseTwin
                },
            ],
            "neither overwrites the other, and nothing below is touched"
        );
    }

    #[test]
    fn a_file_where_a_folder_is_is_left_alone_both_ways() {
        let source = [
            file("Camera", 1, 1),
            folder("Notes"),
            file("Notes/a.txt", 1, 1),
        ];
        let target = [
            folder("Camera"),
            file("Camera/keep.jpg", 1, 1),
            file("Notes", 1, 1),
        ];

        assert_eq!(
            compare(&source, &target, true),
            [
                Action::Conflict {
                    path: "Camera".into(),
                    why: Clash::FileAndFolder
                },
                Action::Conflict {
                    path: "Notes".into(),
                    why: Clash::FileAndFolder
                },
            ]
        );
    }

    #[test]
    fn hidden_things_and_the_apps_own_files_are_never_copied_nor_removed() {
        let source = [
            folder(".thumbnails"),
            file(".thumbnails/t.jpg", 1, 1),
            file(".nomedia", 0, 1),
            file("a.jpg", 1, 1),
        ];
        let target = [
            file(".DS_Store", 1, 1),
            file("._a.jpg", 1, 1),
            file(".a.jpg.open-mtp-client-part", 1, 1),
            folder(".open-mtp-client-mirror"),
            file(".open-mtp-client-mirror/b.jpg", 1, 1),
            folder("Sub"),
            file("Sub/.DS_Store", 1, 1),
            file("a.jpg", 1, 1),
        ];

        assert_eq!(
            compare(&source, &target, true),
            [Action::Extra {
                target: "Sub".into(),
                is_folder: true,
                remove: true
            }]
        );
    }

    #[test]
    fn without_a_date_the_size_decides_and_an_unknown_size_the_date() {
        let undated = |size| Item {
            modified: None,
            ..file("a.mov", size, 0)
        };
        assert!(is_same(&undated(5), &file("a.mov", 5, 77)));
        assert!(!is_same(&undated(5), &file("a.mov", 6, 77)));
        // MTP's 32-bit size field: "4 GB or more".
        assert!(is_same(
            &file("a.mov", 0xFFFF_FFFF, 7),
            &file("a.mov", 6_000_000_000, 7)
        ));
        assert!(!is_same(
            &file("a.mov", 0xFFFF_FFFF, 7),
            &file("a.mov", 6_000_000_000, 99)
        ));
        // The phone as the target, in a mirror from the Mac.
        assert!(is_same(
            &file("a.mov", 6_000_000_000, 7),
            &file("a.mov", 0xFFFF_FFFF, 7)
        ));
    }

    #[test]
    fn two_target_names_that_differ_only_in_case_are_left_alone_or_are_both_extra() {
        let source = [file("a.jpg", 1, 1)];
        let target = [
            file("a.jpg", 1, 1),
            file("A.jpg", 2, 2),
            file("b.jpg", 1, 1),
            file("B.jpg", 1, 1),
        ];

        assert_eq!(
            compare(&source, &target, true),
            [
                Action::Conflict {
                    path: "a.jpg".into(),
                    why: Clash::CaseTwin
                },
                Action::Extra {
                    target: "B.jpg".into(),
                    is_folder: false,
                    remove: true
                },
                Action::Extra {
                    target: "b.jpg".into(),
                    is_folder: false,
                    remove: true
                },
            ]
        );
    }
}
