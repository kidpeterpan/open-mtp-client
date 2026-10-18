//! `transfers.json`: the transfers not finished when the app last ended, so
//! they can be offered again. Its own format, apart from `dto::SavedJob`, so
//! the entity stays free of serde. The file is anyone's to edit, so what
//! can't be a transfer is left out rather than trusted.

use serde::{Deserialize, Serialize};

use crate::dto::{
    CheckRequest, Conflict, DownloadRequest, ImportRequest, MirrorDirection, MirrorRequest, Paste,
    PasteRequest, SavedAnswer, SavedJob, SavedTransfer, UploadRequest,
};

/// The most transfers kept: a queue longer than this is not a real one.
pub const MOST_SAVED: usize = 100;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TransfersFile {
    version: u32,
    transfers: Vec<EntryFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phone: Option<String>,
    #[serde(flatten)]
    job: JobFile,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum JobFile {
    Download {
        storage: u64,
        source: String,
        names: Vec<String>,
        destination: String,
        moving: bool,
        answer: AnswerFile,
    },
    Upload {
        storage: u64,
        sources: Vec<String>,
        destination: String,
        moving: bool,
        answer: AnswerFile,
    },
    OnPhone {
        moving: bool,
        from_storage: u64,
        from: String,
        names: Vec<String>,
        to_storage: u64,
        to: String,
        resuming: bool,
    },
    OnMac {
        moving: bool,
        from: String,
        names: Vec<String>,
        to: String,
    },
    Import {
        storage: u64,
        destination: String,
        /// Written for older versions, read when `folders` is missing.
        #[serde(default)]
        by_date: bool,
        #[serde(default)]
        folders: String,
        remove_from_phone: bool,
    },
    Mirror {
        storage: u64,
        phone_folder: String,
        mac_folder: String,
        to_phone: bool,
        remove_extras: bool,
    },
    Check {
        storage: u64,
        phone_folder: String,
        mac_folder: String,
        names: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AnswerFile {
    Unasked,
    NothingTaken,
    Chose(ConflictFile),
    Retried(ConflictFile),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ConflictFile {
    Skip,
    KeepBoth,
    Replace,
    ReplaceOlder,
    Merge,
}

fn to_conflict_file(conflict: Conflict) -> ConflictFile {
    match conflict {
        Conflict::Skip => ConflictFile::Skip,
        Conflict::KeepBoth => ConflictFile::KeepBoth,
        Conflict::Replace => ConflictFile::Replace,
        Conflict::ReplaceOlder => ConflictFile::ReplaceOlder,
        Conflict::Merge => ConflictFile::Merge,
    }
}

fn to_conflict(conflict: ConflictFile) -> Conflict {
    match conflict {
        ConflictFile::Skip => Conflict::Skip,
        ConflictFile::KeepBoth => Conflict::KeepBoth,
        ConflictFile::Replace => Conflict::Replace,
        ConflictFile::ReplaceOlder => Conflict::ReplaceOlder,
        ConflictFile::Merge => Conflict::Merge,
    }
}

fn to_answer_file(answer: SavedAnswer) -> AnswerFile {
    match answer {
        SavedAnswer::Unasked => AnswerFile::Unasked,
        SavedAnswer::NothingTaken => AnswerFile::NothingTaken,
        SavedAnswer::Chose(conflict) => AnswerFile::Chose(to_conflict_file(conflict)),
        SavedAnswer::Retried(conflict) => AnswerFile::Retried(to_conflict_file(conflict)),
    }
}

fn to_answer(answer: AnswerFile) -> SavedAnswer {
    match answer {
        AnswerFile::Unasked => SavedAnswer::Unasked,
        AnswerFile::NothingTaken => SavedAnswer::NothingTaken,
        AnswerFile::Chose(conflict) => SavedAnswer::Chose(to_conflict(conflict)),
        AnswerFile::Retried(conflict) => SavedAnswer::Retried(to_conflict(conflict)),
    }
}

fn mode_of(moving: bool) -> Paste {
    if moving { Paste::Move } else { Paste::Copy }
}

pub fn to_transfers_file(saved: &[SavedTransfer]) -> TransfersFile {
    TransfersFile {
        version: 1,
        transfers: saved
            .iter()
            .take(MOST_SAVED)
            .map(|saved| EntryFile {
                phone: saved.phone.clone(),
                job: to_job_file(&saved.job),
            })
            .collect(),
    }
}

fn to_job_file(job: &SavedJob) -> JobFile {
    match job {
        SavedJob::Download {
            request,
            mode,
            answer,
        } => JobFile::Download {
            storage: request.storage_id,
            source: request.source.clone(),
            names: request.names.clone(),
            destination: request.destination.clone(),
            moving: *mode == Paste::Move,
            answer: to_answer_file(*answer),
        },
        SavedJob::Upload {
            request,
            mode,
            answer,
        } => JobFile::Upload {
            storage: request.storage_id,
            sources: request.sources.clone(),
            destination: request.destination.clone(),
            moving: *mode == Paste::Move,
            answer: to_answer_file(*answer),
        },
        SavedJob::OnPhone { request, resuming } => JobFile::OnPhone {
            moving: request.mode == Paste::Move,
            from_storage: request.from_storage,
            from: request.from.clone(),
            names: request.names.clone(),
            to_storage: request.to_storage,
            to: request.to.clone(),
            resuming: *resuming,
        },
        SavedJob::OnMac(request) => JobFile::OnMac {
            moving: request.mode == Paste::Move,
            from: request.from.clone(),
            names: request.names.clone(),
            to: request.to.clone(),
        },
        SavedJob::Import(request) => JobFile::Import {
            storage: request.storage_id,
            destination: request.destination.clone(),
            by_date: request.folders != crate::dto::ImportFolders::None,
            folders: super::mapper::folders_name(request.folders).to_owned(),
            remove_from_phone: request.remove_from_phone,
        },
        SavedJob::Mirror(request) => JobFile::Mirror {
            storage: request.storage_id,
            phone_folder: request.phone_folder.clone(),
            mac_folder: request.mac_folder.clone(),
            to_phone: request.direction == MirrorDirection::ToPhone,
            remove_extras: request.remove_extras,
        },
        SavedJob::Check(request) => JobFile::Check {
            storage: request.storage_id,
            phone_folder: request.phone_folder.clone(),
            mac_folder: request.mac_folder.clone(),
            names: request.names.clone(),
        },
    }
}

/// What the file says, leaving out what can't be a transfer: no items, a
/// place that isn't a path from the top, or a phone job with no phone.
pub fn to_transfers(file: TransfersFile) -> Vec<SavedTransfer> {
    file.transfers
        .into_iter()
        .filter_map(|entry| {
            let job = to_job(entry.job)?;
            let for_phone = !matches!(job, SavedJob::OnMac(_));
            if for_phone && entry.phone.as_deref().is_none_or(str::is_empty) {
                return None;
            }
            Some(SavedTransfer {
                phone: entry.phone.filter(|_| for_phone),
                job,
            })
        })
        .take(MOST_SAVED)
        .collect()
}

fn absolute(path: &str) -> bool {
    path.starts_with('/')
}

fn to_job(job: JobFile) -> Option<SavedJob> {
    let job = match job {
        JobFile::Download {
            storage,
            source,
            names,
            destination,
            moving,
            answer,
        } => {
            if names.is_empty() || !absolute(&source) || !absolute(&destination) {
                return None;
            }
            SavedJob::Download {
                request: DownloadRequest {
                    storage_id: storage,
                    source,
                    names,
                    destination,
                },
                mode: mode_of(moving),
                answer: to_answer(answer),
            }
        }
        JobFile::Upload {
            storage,
            sources,
            destination,
            moving,
            answer,
        } => {
            if sources.is_empty()
                || !sources.iter().all(|source| absolute(source))
                || !absolute(&destination)
            {
                return None;
            }
            SavedJob::Upload {
                request: UploadRequest {
                    sources,
                    storage_id: storage,
                    destination,
                },
                mode: mode_of(moving),
                answer: to_answer(answer),
            }
        }
        JobFile::OnPhone {
            moving,
            from_storage,
            from,
            names,
            to_storage,
            to,
            resuming,
        } => {
            if names.is_empty() || !absolute(&from) || !absolute(&to) {
                return None;
            }
            SavedJob::OnPhone {
                request: PasteRequest {
                    mode: mode_of(moving),
                    from_storage,
                    from,
                    names,
                    to_storage,
                    to,
                },
                resuming,
            }
        }
        JobFile::OnMac {
            moving,
            from,
            names,
            to,
        } => {
            if names.is_empty() || !absolute(&from) || !absolute(&to) {
                return None;
            }
            SavedJob::OnMac(PasteRequest {
                mode: mode_of(moving),
                from_storage: 0,
                from,
                names,
                to_storage: 0,
                to,
            })
        }
        JobFile::Import {
            storage,
            destination,
            by_date,
            folders,
            remove_from_phone,
        } => {
            if !absolute(&destination) {
                return None;
            }
            SavedJob::Import(ImportRequest {
                storage_id: storage,
                destination,
                folders: super::mapper::folders_of(&folders, by_date),
                remove_from_phone,
            })
        }
        JobFile::Mirror {
            storage,
            phone_folder,
            mac_folder,
            to_phone,
            remove_extras,
        } => {
            if !absolute(&phone_folder) || !absolute(&mac_folder) {
                return None;
            }
            SavedJob::Mirror(MirrorRequest {
                storage_id: storage,
                phone_folder,
                mac_folder,
                direction: if to_phone {
                    MirrorDirection::ToPhone
                } else {
                    MirrorDirection::ToMac
                },
                remove_extras,
            })
        }
        JobFile::Check {
            storage,
            phone_folder,
            mac_folder,
            names,
        } => {
            if names.is_empty() || !absolute(&phone_folder) || !absolute(&mac_folder) {
                return None;
            }
            SavedJob::Check(CheckRequest {
                storage_id: storage,
                phone_folder,
                mac_folder,
                names,
            })
        }
    };
    Some(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Vec<SavedTransfer> {
        let phone = Some("ABC123".to_owned());
        vec![
            SavedTransfer {
                phone: phone.clone(),
                job: SavedJob::Download {
                    request: DownloadRequest {
                        storage_id: 65_537,
                        source: "/DCIM".into(),
                        names: vec!["Camera".into()],
                        destination: "/Users/me/Pictures".into(),
                    },
                    mode: Paste::Move,
                    answer: SavedAnswer::Retried(Conflict::ReplaceOlder),
                },
            },
            SavedTransfer {
                phone: phone.clone(),
                job: SavedJob::Upload {
                    request: UploadRequest {
                        sources: vec!["/Users/me/a.jpg".into()],
                        storage_id: 65_537,
                        destination: "/Download".into(),
                    },
                    mode: Paste::Copy,
                    answer: SavedAnswer::Chose(Conflict::Skip),
                },
            },
            SavedTransfer {
                phone: phone.clone(),
                job: SavedJob::OnPhone {
                    request: PasteRequest {
                        mode: Paste::Move,
                        from_storage: 1,
                        from: "/DCIM".into(),
                        names: vec!["a.jpg".into()],
                        to_storage: 2,
                        to: "/Old".into(),
                    },
                    resuming: true,
                },
            },
            SavedTransfer {
                phone: None,
                job: SavedJob::OnMac(PasteRequest {
                    mode: Paste::Copy,
                    from_storage: 0,
                    from: "/Users/me".into(),
                    names: vec!["big.mov".into()],
                    to_storage: 0,
                    to: "/Volumes/Backup".into(),
                }),
            },
            SavedTransfer {
                phone: phone.clone(),
                job: SavedJob::Import(ImportRequest {
                    storage_id: 65_537,
                    destination: "/Users/me/Pictures".into(),
                    folders: crate::dto::ImportFolders::Day,
                    remove_from_phone: true,
                }),
            },
            SavedTransfer {
                phone: phone.clone(),
                job: SavedJob::Mirror(MirrorRequest {
                    storage_id: 65_537,
                    phone_folder: "/DCIM/Camera".into(),
                    mac_folder: "/Users/me/Camera".into(),
                    direction: MirrorDirection::ToPhone,
                    remove_extras: false,
                }),
            },
            SavedTransfer {
                phone,
                job: SavedJob::Check(CheckRequest {
                    storage_id: 65_537,
                    phone_folder: "/".into(),
                    mac_folder: "/Users/me/Backup".into(),
                    names: vec!["DCIM".into(), "notes.txt".into()],
                }),
            },
        ]
    }

    #[test]
    fn every_kind_of_transfer_comes_back_as_it_was_kept() {
        let kept = every_kind();
        let json = serde_json::to_string(&to_transfers_file(&kept)).unwrap();

        let back = to_transfers(serde_json::from_str(&json).unwrap());

        assert_eq!(back, kept);
        assert!(json.contains("\"kind\":\"download\""), "{json}");
        assert!(json.contains("\"retried\":\"replaceOlder\""), "{json}");
        let answers = [
            SavedAnswer::Unasked,
            SavedAnswer::NothingTaken,
            SavedAnswer::Chose(Conflict::KeepBoth),
            SavedAnswer::Retried(Conflict::Merge),
            SavedAnswer::Chose(Conflict::Replace),
        ];
        for answer in answers {
            assert_eq!(to_answer(to_answer_file(answer)), answer);
        }
    }

    #[test]
    fn what_cannot_be_a_transfer_is_left_out_of_a_hand_edited_file() {
        let json = r#"{"version":1,"transfers":[
            {"phone":"ABC","kind":"download","storage":1,"source":"/DCIM","names":[],"destination":"/Users/me","moving":false,"answer":"unasked"},
            {"phone":"ABC","kind":"download","storage":1,"source":"DCIM","names":["a"],"destination":"/Users/me","moving":false,"answer":"unasked"},
            {"phone":"ABC","kind":"upload","storage":1,"sources":["relative.jpg"],"destination":"/","moving":false,"answer":"unasked"},
            {"phone":"ABC","kind":"upload","storage":1,"sources":[],"destination":"/","moving":false,"answer":"unasked"},
            {"kind":"import","storage":1,"destination":"/Users/me","byDate":false,"removeFromPhone":false},
            {"phone":"","kind":"mirror","storage":1,"phoneFolder":"/a","macFolder":"/b","toPhone":false,"removeExtras":false},
            {"phone":"ABC","kind":"onMac","moving":true,"from":"/a","names":["x"],"to":"b"},
            {"phone":"ABC","kind":"check","storage":1,"phoneFolder":"/","macFolder":"/b","names":[]},
            {"phone":"ABC","kind":"check","storage":1,"phoneFolder":"/","macFolder":"b","names":["x"]},
            {"phone":"ABC","kind":"check","storage":1,"phoneFolder":"DCIM","macFolder":"/b","names":["x"]},
            {"phone":"ABC","kind":"teleport"},
            {"phone":"ABC","kind":"onMac","moving":true,"from":"/a","names":["x"],"to":"/b"}
        ]}"#;
        let file: Result<TransfersFile, _> = serde_json::from_str(json);
        // An unknown kind makes the whole list unreadable: nothing is trusted.
        assert!(file.is_err());

        let without_unknown = json.replace(r#"{"phone":"ABC","kind":"teleport"},"#, "");
        let kept = to_transfers(serde_json::from_str(&without_unknown).unwrap());
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].phone, None, "a Mac paste belongs to no phone");
        assert!(matches!(kept[0].job, SavedJob::OnMac(_)));
    }

    #[test]
    fn an_import_kept_before_its_folders_had_a_name_comes_back_by_day() {
        let json = r#"{"version":1,"transfers":[
            {"phone":"ABC","kind":"import","storage":1,"destination":"/Users/me","byDate":true,"removeFromPhone":false},
            {"phone":"ABC","kind":"import","storage":1,"destination":"/Users/me","byDate":false,"removeFromPhone":false},
            {"phone":"ABC","kind":"import","storage":1,"destination":"/Users/me","folders":"yearMonth","removeFromPhone":false}
        ]}"#;
        let kept = to_transfers(serde_json::from_str(json).unwrap());
        let folders: Vec<crate::dto::ImportFolders> = kept
            .iter()
            .map(|saved| match &saved.job {
                SavedJob::Import(request) => request.folders,
                _ => panic!("{saved:?}"),
            })
            .collect();
        assert_eq!(
            folders,
            [
                crate::dto::ImportFolders::Day,
                crate::dto::ImportFolders::None,
                crate::dto::ImportFolders::YearMonth
            ]
        );
    }

    #[test]
    fn no_more_than_a_hundred_are_kept() {
        let many: Vec<SavedTransfer> =
            std::iter::repeat_n(every_kind()[3].clone(), MOST_SAVED + 5).collect();
        let file = to_transfers_file(&many);
        assert_eq!(file.transfers.len(), MOST_SAVED);
        let json = serde_json::to_string(&TransfersFile {
            version: 1,
            transfers: many
                .iter()
                .map(|saved| EntryFile {
                    phone: None,
                    job: to_job_file(&saved.job),
                })
                .collect(),
        })
        .unwrap();
        assert_eq!(
            to_transfers(serde_json::from_str(&json).unwrap()).len(),
            MOST_SAVED
        );
    }
}
