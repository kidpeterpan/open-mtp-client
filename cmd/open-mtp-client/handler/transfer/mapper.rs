use std::path::Path;

use internal::dto;

use super::response::{ProgressResponse, ReportRow, SummaryResponse};
use crate::handler::error;
use crate::handler::format::{
    comma_list, escape_odd, format_bytes, format_count, format_duration, format_elapsed,
};
use crate::i18n::{count, key, t, tf};

/// Speed and time left are noise until a moment of data has come in.
const MIN_SECONDS_FOR_ESTIMATE: f64 = 1.0;

/// From this much to copy, the phone is asked to stay unlocked and plugged in:
/// a copy that long is exactly when a locked screen stalls it.
const KEEP_UNLOCKED_FROM: u64 = 100_000_000;

/// The most failures spelled out in a result; the rest are counted.
const MAX_FAILURES_SHOWN: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Doing {
    Transfer,
    Import,
    Mirror,
    Check,
}

fn waiting(title: &str, detail: &str, note: Option<String>) -> ProgressResponse {
    ProgressResponse {
        title: title.to_owned(),
        current_file: String::new(),
        file_fraction: 0.0,
        total_fraction: 0.0,
        detail: detail.to_owned(),
        note,
    }
}

fn joined(parts: Vec<String>) -> String {
    parts.join(" · ")
}

/// Shown while the selection is being counted, before the first byte.
pub fn preparing_response(direction: dto::Direction, moving: bool) -> ProgressResponse {
    let title = match (moving, direction) {
        (true, _) => key("transfer.preparing.move"),
        (false, dto::Direction::Download) => key("transfer.preparing.download"),
        (false, dto::Direction::Upload) => key("transfer.preparing.upload"),
        (false, dto::Direction::Local) => key("transfer.preparing.copy"),
    };
    waiting(t(title), t("transfer.counting_files"), None)
}

/// Shown while an import looks for what is new.
pub fn preparing_import() -> ProgressResponse {
    waiting(
        t("transfer.preparing.import"),
        t("transfer.preparing.import_detail"),
        None,
    )
}

/// An import's progress: a download's, said as importing.
pub fn to_import_progress(progress: &dto::TransferProgress) -> ProgressResponse {
    progress_response(progress, Doing::Import)
}

/// How an import went: what was new, and what was there already.
pub fn to_import_summary(summary: &dto::TransferSummary) -> SummaryResponse {
    // An import that freed the phone is still an import, never a "move".
    let copied = dto::TransferSummary {
        removal: None,
        ..summary.clone()
    };
    let mut response = summary_response(&copied, key("transfer.part.imported_before"));
    if let Some(removal) = &summary.removal {
        if removal.removed > 0 {
            let removed = tf(
                "transfer.import.removed",
                &[
                    ("files", &files(removal.removed)),
                    ("size", &format_bytes(removal.freed)),
                ],
            );
            response.detail = joined(vec![response.detail, removed]);
        }
        response.notes = removal_notes(removal);
    }
    if summary.failures.is_empty() {
        response.title = match summary.files {
            0 => t("transfer.import.none").to_owned(),
            n => count(
                "transfer.import.title",
                n,
                &[("size", &format_bytes(summary.bytes))],
            ),
        };
    }
    response
}

/// Shown while a mirror compares the two folders.
pub fn preparing_mirror() -> ProgressResponse {
    waiting(
        t("transfer.preparing.mirror"),
        t("transfer.preparing.mirror_detail"),
        None,
    )
}

/// A mirror's progress: a download's or an upload's, said as mirroring.
pub fn to_mirror_progress(progress: &dto::TransferProgress) -> ProgressResponse {
    progress_response(progress, Doing::Mirror)
}

/// How a mirror went: copied, replaced, removed, kept and left alone.
pub fn to_mirror_summary(summary: &dto::TransferSummary) -> SummaryResponse {
    let mut response = to_summary_response(summary);
    let mirrored = summary.mirrored.clone().unwrap_or_default();
    let onto_phone = summary.direction == dto::Direction::Upload;
    let mut done = Vec::new();
    if mirrored.copied > 0 {
        done.push(tf(
            "transfer.mirror.copied",
            &[("files", &files(mirrored.copied))],
        ));
    }
    if mirrored.replaced > 0 {
        done.push(tf(
            "transfer.mirror.replaced",
            &[("files", &files(mirrored.replaced))],
        ));
    }
    if mirrored.removed > 0 {
        done.push(tf(
            "transfer.mirror.removed",
            &[("items", &items(mirrored.removed))],
        ));
    }
    response.title = match (done.is_empty(), summary.failures.is_empty()) {
        (true, true) => t("transfer.mirror.same").to_owned(),
        (true, false) => t("transfer.mirror.nothing").to_owned(),
        (false, _) => tf("transfer.mirror.title", &[("done", &comma_list(&done))]),
    };
    let folder = folder_name(&summary.destination);
    let place = if onto_phone {
        key("transfer.mirror.onto")
    } else {
        key("transfer.mirror.into")
    };
    let mut parts = vec![tf(place, &[("folder", &folder)])];
    if mirrored.removed > 0 {
        parts.push(
            if onto_phone {
                t("transfer.mirror.part_removed_phone")
            } else {
                t("transfer.mirror.part_removed_trash")
            }
            .to_owned(),
        );
    } else if mirrored.replaced > 0 && !onto_phone {
        parts.push(t("transfer.mirror.part_old_versions").to_owned());
    }
    if mirrored.kept > 0 {
        parts.push(count("transfer.mirror.part_only_there", mirrored.kept, &[]));
    }
    if mirrored.conflicts > 0 {
        parts.push(count("transfer.mirror.part_clash", mirrored.conflicts, &[]));
    }
    if !summary.failures.is_empty() {
        parts.push(count("transfer.part.failed", summary.failures.len(), &[]));
    }
    response.detail = joined(parts);
    response
}

/// The queue's name for a mirror.
pub fn mirror_label(request: &dto::MirrorRequest) -> String {
    match request.direction {
        dto::MirrorDirection::ToMac => tf(
            "transfer.mirror.label_to_mac",
            &[("folder", &folder_name(&request.phone_folder))],
        ),
        dto::MirrorDirection::ToPhone => tf(
            "transfer.mirror.label_to_phone",
            &[("folder", &folder_name(&request.mac_folder))],
        ),
    }
}

/// The queue's name for a delete that keeps copies: "Delete “a.jpg”,
/// keeping a copy in the Trash".
pub fn delete_keeping_label(names: &[String]) -> String {
    match names {
        [one] => tf("transfer.delete.label_one", &[("name", one)]),
        names => tf(
            "transfer.delete.label_many",
            &[("items", &items(names.len()))],
        ),
    }
}

/// How a delete that keeps copies went: what left the phone, and what
/// stayed and why.
pub fn to_delete_summary(summary: &dto::TransferSummary) -> SummaryResponse {
    let mut response = to_summary_response(summary);
    let removal = summary.removal.clone().unwrap_or_default();
    response.title = match removal.removed {
        0 => t("transfer.delete.nothing").to_owned(),
        n => tf("transfer.delete.done", &[("files", &files(n))]),
    };
    response.detail = t("transfer.delete.detail").to_owned();
    response.notes = removal
        .kept
        .iter()
        .map(|kept| {
            tf(
                "transfer.delete.kept_note",
                &[("label", &kept.label), ("why", &why(&kept.why))],
            )
        })
        .chain(removal.stopped.iter().map(|stopped| {
            tf(
                "transfer.delete.stopped",
                &[("reason", &error::describe_again(stopped).title)],
            )
        }))
        .collect();
    response.left = Vec::new();
    response.reveal = None;
    response.report_folder = None;
    response
}

/// The queue's name for sending an edit back: "Send “notes.txt” back to
/// the phone".
pub fn send_edit_label(name: &str) -> String {
    tf("transfer.edit.send_label", &[("name", name)])
}

/// A working copy saved on the Mac: "notes.txt was changed on the Mac".
pub fn edit_saved_title(name: &str) -> String {
    tf("transfer.edit.saved_title", &[("name", name)])
}

/// A working copy not changed yet.
pub fn edit_open_title(name: &str) -> String {
    tf("transfer.edit.open_title", &[("name", name)])
}

/// Under a saved edit, with its phone open.
pub fn edit_send_note() -> &'static str {
    t("transfer.edit.send_note")
}

/// Under a saved edit whose phone isn't open.
pub fn edit_phone_note() -> &'static str {
    t("transfer.edit.phone_note")
}

/// Under a working copy not changed yet.
pub fn edit_open_note() -> &'static str {
    t("transfer.edit.open_note")
}

/// How sending an edit went: over the phone's file, or beside it.
pub fn to_edit_summary(summary: &dto::TransferSummary, name: &str) -> SummaryResponse {
    let mut response = to_summary_response(summary);
    let beside = summary
        .report
        .first()
        .map(|line| line.path.clone())
        .filter(|sent| sent != name);
    (response.title, response.detail) = match beside {
        Some(sent) => (
            tf("transfer.edit.sent_beside", &[("sent", &sent)]),
            tf("transfer.edit.sent_beside_detail", &[("name", name)]),
        ),
        None => (
            tf("transfer.edit.sent", &[("name", name)]),
            t("transfer.edit.sent_detail").to_owned(),
        ),
    };
    response.reveal = None;
    response.report_folder = None;
    response
}

/// The queue's name for a check of contents: "Check 3 items against Backup".
pub fn check_label(request: &dto::CheckRequest) -> String {
    let folder = folder_name(&request.mac_folder);
    match request.names.as_slice() {
        [one] => tf(
            "transfer.check.label_one",
            &[("name", one), ("folder", &folder)],
        ),
        names => tf(
            "transfer.check.label_many",
            &[("items", &items(names.len())), ("folder", &folder)],
        ),
    }
}

/// Shown while a check finds the files to compare.
pub fn preparing_check() -> ProgressResponse {
    waiting(
        t("transfer.preparing.check"),
        t("transfer.preparing.check_detail"),
        Some(check_note().to_owned()),
    )
}

/// Under a check while it runs: what it costs and what it leaves alone.
pub fn check_note() -> &'static str {
    t("transfer.check_note")
}

/// A check's progress: a download's, said as checking.
pub fn to_check_progress(progress: &dto::TransferProgress) -> ProgressResponse {
    let mut response = progress_response(progress, Doing::Check);
    response.note = Some(check_note().to_owned());
    response
}

/// How a check went: "All 4 files identical", or "Checked 4 files: 1
/// differs, 3 identical", with the details file by file.
pub fn to_check_summary(summary: &dto::TransferSummary) -> SummaryResponse {
    let mut response = to_summary_response(summary);
    let compared = summary.compared.unwrap_or_default();
    let total = compared.total();
    let mut found = Vec::new();
    if compared.differ > 0 {
        found.push(count("transfer.check.differ", compared.differ, &[]));
    }
    if compared.missing > 0 {
        found.push(count("transfer.check.one_side", compared.missing, &[]));
    }
    if compared.unreadable > 0 {
        found.push(count("transfer.check.unreadable", compared.unreadable, &[]));
    }
    if compared.identical > 0 {
        found.push(count("transfer.check.identical", compared.identical, &[]));
    }
    response.title = if total == 0 {
        t("transfer.check.nothing").to_owned()
    } else if compared.identical == total {
        match total {
            1 => t("transfer.check.single_identical").to_owned(),
            n => tf("transfer.check.all_identical", &[("files", &files(n))]),
        }
    } else {
        tf(
            "transfer.check.title",
            &[("files", &files(total)), ("found", &comma_list(&found))],
        )
    };
    response.detail = tf(
        "transfer.check.detail",
        &[("folder", &folder_name(&summary.destination))],
    );
    response.mismatched = compared.differ + compared.missing > 0;
    // Nothing arrived anywhere: no Finder, and no report file written
    // into a folder that was only looked at (Copy still works).
    response.reveal = None;
    response.report_folder = None;
    response
}

/// The queue's name for an import.
pub fn import_label(destination: &str) -> String {
    tf(
        "transfer.import.label",
        &[("folder", &folder_name(destination))],
    )
}

/// Shown while a copy through the Mac is being counted.
pub fn preparing_via_mac(moving: bool) -> ProgressResponse {
    let title = if moving {
        key("transfer.preparing.via_mac_move")
    } else {
        key("transfer.preparing.via_mac_copy")
    };
    waiting(t(title), t("transfer.counting_files"), None)
}

pub fn to_progress_response(progress: &dto::TransferProgress) -> ProgressResponse {
    progress_response(progress, Doing::Transfer)
}

fn progress_title(progress: &dto::TransferProgress, doing: Doing, current: usize) -> String {
    let message = if progress.through_mac {
        match (progress.moving, progress.direction) {
            (false, dto::Direction::Download) => key("transfer.progress.mac_copy_reading"),
            (false, dto::Direction::Upload) => key("transfer.progress.mac_copy_sending"),
            // (Nothing inside the Mac goes by way of the Mac.)
            (false, dto::Direction::Local) => key("transfer.progress.mac_copy_copying"),
            (true, dto::Direction::Download) => key("transfer.progress.mac_move_reading"),
            (true, dto::Direction::Upload) => key("transfer.progress.mac_move_sending"),
            (true, dto::Direction::Local) => key("transfer.progress.mac_move_copying"),
        }
    } else if progress.moving {
        key("transfer.progress.moving")
    } else {
        match (doing, progress.direction) {
            (Doing::Import, dto::Direction::Download) => key("transfer.progress.importing"),
            (Doing::Mirror, dto::Direction::Download | dto::Direction::Upload) => {
                key("transfer.progress.mirroring")
            }
            (Doing::Check, dto::Direction::Download) => key("transfer.progress.checking"),
            (_, dto::Direction::Download) => key("transfer.progress.downloading"),
            (_, dto::Direction::Upload) => key("transfer.progress.uploading"),
            (_, dto::Direction::Local) => key("transfer.progress.copying"),
        }
    };
    tf(
        message,
        &[
            ("current", &format_count(current)),
            ("total", &format_count(progress.files_total)),
        ],
    )
}

fn progress_response(progress: &dto::TransferProgress, doing: Doing) -> ProgressResponse {
    if progress.removing {
        return removing_response(progress);
    }
    let current = (progress.files_done + 1).min(progress.files_total.max(1));
    let done = format_bytes(progress.bytes_done);
    let total = format_bytes(progress.bytes_total);
    let seconds = progress.elapsed.as_secs_f64();
    let detail = if seconds >= MIN_SECONDS_FOR_ESTIMATE && progress.bytes_done > 0 {
        let speed = progress.bytes_done as f64 / seconds;
        let left = progress.bytes_total.saturating_sub(progress.bytes_done) as f64 / speed;
        tf(
            "transfer.detail.speed",
            &[
                ("done", &done),
                ("total", &total),
                ("speed", &format_bytes(speed as u64)),
                ("left", &format_duration(left.ceil() as u64)),
            ],
        )
    } else {
        tf(
            "transfer.detail.estimating",
            &[("done", &done), ("total", &total)],
        )
    };

    ProgressResponse {
        title: progress_title(progress, doing, current),
        // All the bytes are in; the phone or the disk is settling the file,
        // which can take a few seconds and must not look like a hang at 100%.
        current_file: if progress.finishing {
            tf("transfer.finishing", &[("file", &progress.current_file)])
        } else {
            progress.current_file.clone()
        },
        file_fraction: fraction(progress.file_bytes_done, progress.file_bytes_total),
        total_fraction: fraction(progress.bytes_done, progress.bytes_total),
        detail,
        note: (progress.bytes_total >= KEEP_UNLOCKED_FROM)
            .then(|| t("transfer.note.keep_unlocked").to_owned()),
    }
}

/// The note under a long copy's bars, saying too that the Mac is kept awake
/// when it is (sleep would drop the phone; the user may mean to walk away).
pub fn with_awake(note: &str, awake: bool) -> String {
    if awake {
        tf(
            "text.sentences",
            &[("first", note), ("second", t("transfer.note.stay_awake"))],
        )
    } else {
        note.to_owned()
    }
}

/// The long copy's note, saying too why it may be slow when the phone is on a
/// link slower than USB 3.
pub fn with_link(note: String, link: Option<dto::UsbSpeed>) -> String {
    match crate::handler::format::slow_link_note(link) {
        Some(slow) => tf("text.sentences", &[("first", &note), ("second", &slow)]),
        None => note,
    }
}

/// The copy is done and the originals are being removed: from the phone, or
/// (a move to the phone) from the Mac, into the Trash, one item at a time.
fn removing_response(progress: &dto::TransferProgress) -> ProgressResponse {
    let fraction = fraction(progress.files_done as u64, progress.files_total as u64);
    let done = format_count(progress.files_done);
    let (title, current_file, detail) = if progress.trashing {
        (
            t("transfer.removing.trash_title"),
            tf(
                "transfer.removing.trash_progress",
                &[("done", &done), ("items", &items(progress.files_total))],
            ),
            // Which copy was checked, and against what, depends on where it went.
            if progress.direction == dto::Direction::Local {
                t("transfer.removing.checked_detail")
            } else {
                t("transfer.removing.trash_detail_phone")
            },
        )
    } else {
        (
            t("transfer.removing.phone_title"),
            tf(
                "transfer.removing.phone_progress",
                &[
                    ("done", &done),
                    ("total", &format_count(progress.files_total)),
                ],
            ),
            t("transfer.removing.checked_detail"),
        )
    };
    ProgressResponse {
        title: title.to_owned(),
        current_file,
        file_fraction: fraction,
        total_fraction: fraction,
        detail: detail.to_owned(),
        note: None,
    }
}

pub fn to_summary_response(summary: &dto::TransferSummary) -> SummaryResponse {
    summary_response(summary, key("transfer.part.already_there"))
}

fn summary_response(
    summary: &dto::TransferSummary,
    already_there_part: &'static str,
) -> SummaryResponse {
    let folder = folder_name(&summary.destination);
    let mut parts = vec![if summary.through_mac {
        tf("transfer.detail.to_via_mac", &[("folder", &folder)])
    } else {
        tf("transfer.detail.to", &[("folder", &folder)])
    }];
    if summary.skipped > 0 {
        parts.push(count("transfer.part.skipped", summary.skipped, &[]));
    }
    if summary.replaced > 0 {
        parts.push(count("transfer.part.replaced", summary.replaced, &[]));
    }
    if summary.merged > 0 {
        parts.push(count("transfer.part.merged", summary.merged, &[]));
    }
    if summary.already_there > 0 {
        parts.push(count(already_there_part, summary.already_there, &[]));
    }
    if !summary.failures.is_empty() {
        parts.push(count("transfer.part.failed", summary.failures.len(), &[]));
    }
    if summary.resumed > 0 {
        parts.push(count("transfer.part.resumed", summary.resumed, &[]));
    }
    if summary.renamed > 0 {
        parts.push(count("transfer.part.renamed", summary.renamed, &[]));
    }

    let removal = summary.removal.as_ref();
    // What a move did with the originals, as a tail on the detail line.
    if let Some(removal) = removal {
        if removal.removed > 0 && !removal.left.is_empty() {
            let part = if removal.from_mac {
                key("transfer.part.moved_trash")
            } else {
                key("transfer.part.removed_phone")
            };
            parts.push(tf(part, &[("files", &files(removal.removed))]));
        }
        if !removal.left.is_empty() {
            let part = if removal.from_mac {
                key("transfer.part.left_on_mac")
            } else {
                key("transfer.part.left_on_phone")
            };
            parts.push(tf(part, &[("items", &items(removal.left.len()))]));
        }
    }
    let detail = joined(parts);
    // A move that left nothing behind has finished moving.
    let moved = removal.is_some_and(|removal| removal.left.is_empty());

    // A retry that found everything in place copied nothing, which is good
    // news and should not read as "Downloaded 0 files".
    let title = if summary.files == 0 && !summary.failures.is_empty() {
        t("transfer.summary.nothing_copied").to_owned()
    } else if summary.through_mac
        && summary.files == 0
        && summary.already_there == 0
        && removal.is_none_or(|removal| removal.removed == 0 && removal.left.is_empty())
    {
        // A move into the folder the items are already in.
        t("transfer.summary.already_here").to_owned()
    } else if moved && summary.files == 0 && summary.already_there > 0 {
        let removed = removal.map_or(0, |removal| removal.removed);
        tf(
            "transfer.summary.moved_already",
            &[("files", &files(removed))],
        )
    } else if summary.files == 0 && summary.already_there > 0 {
        t("transfer.summary.nothing_new").to_owned()
    } else {
        let message = match (summary.direction, removal) {
            (_, Some(_)) if moved => key("transfer.summary.moved"),
            // Copied, but not all of it moved.
            (_, Some(_)) => key("transfer.summary.copied"),
            // Inside the phone, nothing was uploaded in the user's eyes.
            (_, None) if summary.through_mac => key("transfer.summary.copied"),
            (dto::Direction::Download, None) => key("transfer.summary.downloaded"),
            (dto::Direction::Upload, None) => key("transfer.summary.uploaded"),
            (dto::Direction::Local, None) => key("transfer.summary.copied"),
        };
        tf(
            message,
            &[
                ("files", &files(summary.files)),
                ("size", &format_bytes(summary.bytes)),
            ],
        )
    };

    SummaryResponse {
        arrival: summary.arrival.as_ref().and_then(arrival_line),
        renamed: summary.renamed,
        title,
        detail,
        failures: failure_lines(&summary.failures),
        notes: removal.map(removal_notes).unwrap_or_default(),
        left: removal
            .map(|removal| removal.left.clone())
            .unwrap_or_default(),
        // Finder can only show what landed on the Mac.
        reveal: summary
            .first_item
            .as_ref()
            // Finder can show what landed on the Mac, whichever way it came.
            .filter(|_| summary.direction != dto::Direction::Upload)
            .map(|item| {
                Path::new(&summary.destination)
                    .join(item)
                    .to_string_lossy()
                    .into_owned()
            }),
        report: report_rows(&summary.report),
        report_left_out: summary.report_left_out,
        report_folder: (summary.direction == dto::Direction::Download
            && !summary.through_mac
            && !summary.report.is_empty())
        .then(|| summary.destination.clone()),
        // The queue adds it: it knows the link and timed the run.
        timing: None,
        mismatched: false,
    }
}

/// What the paused transfer says under its bar.
pub fn paused_note() -> &'static str {
    t("transfer.paused_note")
}

/// "Paused: Downloading 3 files".
pub fn paused_title(title: &str) -> String {
    tf("transfer.paused_title", &[("title", title)])
}

/// Above the transfers not finished last time.
pub fn offered_title(number: usize) -> String {
    count("transfer.offered_title", number, &[])
}

/// Under them.
pub fn offered_note() -> &'static str {
    t("transfer.offered_note")
}

/// The title of a transfer stopped by losing the phone.
pub fn waiting_title(title: &str) -> String {
    tf("transfer.waiting_title", &[("title", title)])
}

/// Under a transfer waiting for the phone.
pub fn waiting_note() -> &'static str {
    t("transfer.waiting_note")
}

/// The notification when a transfer stops because the phone was lost.
pub fn phone_lost_note(label: &str) -> (String, String) {
    (
        t("transfer.phone_lost.title").to_owned(),
        tf("transfer.phone_lost.body", &[("label", label)]),
    )
}

/// Whether a job ended badly: stopped, or finished with failures or with
/// originals left behind.
pub fn is_troubled(outcome: &Result<SummaryResponse, error::ErrorResponse>) -> bool {
    match outcome {
        Ok(summary) => {
            !summary.failures.is_empty() || !summary.left.is_empty() || summary.mismatched
        }
        Err(_) => true,
    }
}

/// The notification for transfers that ended while the user was in another
/// app: one transfer says how it went in its own words; several are counted,
/// with how many had problems. `None` when there is nothing to tell.
pub fn batch_note(
    batch: &[(String, &Result<SummaryResponse, error::ErrorResponse>)],
) -> Option<(String, String)> {
    let stopped = |label: &str| tf("transfer.batch.stopped", &[("label", label)]);
    match batch {
        [] => None,
        [(label, Err(error))] => Some((stopped(label), error.title.clone())),
        [(_, Ok(summary))] => {
            let body = match &summary.arrival {
                Some(arrival) if !is_troubled(&Ok(summary.clone())) => arrival.clone(),
                _ => summary.detail.clone(),
            };
            Some((summary.title.clone(), body))
        }
        several => {
            let problems = several
                .iter()
                .filter(|(_, outcome)| is_troubled(outcome))
                .count();
            let title = match problems {
                0 => count("transfer.batch.title", several.len(), &[]),
                1 => count("transfer.batch.one_problem", several.len(), &[]),
                n => tf(
                    "transfer.batch.problems",
                    &[
                        ("count", &format_count(several.len())),
                        ("problems", &format_count(n)),
                    ],
                ),
            };
            let body = several
                .iter()
                .take(3)
                .map(|(label, outcome)| match outcome {
                    Ok(summary) => summary.title.clone(),
                    Err(_) => stopped(label),
                })
                .collect::<Vec<_>>()
                .join(" · ");
            Some((title, body))
        }
    }
}

/// The notification for an upload waiting on the question about names that
/// are taken.
pub fn question_note(taken: usize) -> (String, String) {
    (
        t("transfer.question.title").to_owned(),
        count("transfer.question.body", taken, &[]),
    )
}

/// What the count at the end says: everything meant to arrive is there (and
/// was checked, or the phone took each one), or how much of it is. Nothing
/// when there were no files.
pub fn arrival_line(arrival: &dto::Arrival) -> Option<String> {
    if arrival.files == 0 {
        return None;
    }
    if arrival.there < arrival.files {
        return Some(tf(
            "transfer.arrival.partial",
            &[
                ("there", &format_count(arrival.there)),
                ("files", &files(arrival.files)),
            ],
        ));
    }
    let what = tf(
        "transfer.arrival.what",
        &[
            ("files", &files(arrival.files)),
            ("size", &format_bytes(arrival.bytes)),
        ],
    );
    let say = |message| tf(message, &[("what", &what)]);
    Some(match arrival.checked {
        _ if !arrival.complete => say(key("transfer.arrival.cycle")),
        None => say(key("transfer.arrival.sent")),
        Some(checked) if checked >= arrival.there => say(key("transfer.arrival.arrived")),
        Some(checked) => tf(
            "transfer.arrival.unchecked",
            &[
                ("what", &what),
                ("count", &format_count(arrival.there - checked)),
            ],
        ),
    })
}

/// Where a move's originals were, for the person who moved them.
fn where_originals_are(removal: &dto::Removal) -> &'static str {
    if removal.from_mac {
        t("transfer.removal.mac")
    } else {
        t("transfer.removal.phone")
    }
}

/// What a move left behind: one line per original that stayed, the first
/// few, then how many more, then how removing ended if it stopped early.
fn removal_notes(removal: &dto::Removal) -> Vec<String> {
    let place = where_originals_are(removal);
    let mut lines: Vec<String> = removal
        .kept
        .iter()
        .take(MAX_FAILURES_SHOWN)
        .map(|kept| {
            tf(
                "transfer.removal.kept_line",
                &[
                    ("label", &kept.label),
                    ("place", place),
                    ("why", &why(&kept.why)),
                ],
            )
        })
        .collect();
    if removal.kept.len() > MAX_FAILURES_SHOWN {
        lines.push(tf(
            "transfer.removal.more",
            &[
                (
                    "count",
                    &format_count(removal.kept.len() - MAX_FAILURES_SHOWN),
                ),
                ("place", place),
            ],
        ));
    }
    match &removal.stopped {
        None => {}
        Some(dto::Error::Cancelled) => {
            lines.push(tf("transfer.removal.cancelled", &[("place", place)]));
        }
        Some(stopped) => lines.push(tf(
            "transfer.removal.stopped",
            &[
                ("reason", &error::describe_again(stopped).title),
                ("place", place),
            ],
        )),
    }
    lines
}

/// An average no faster than a USB 2 link goes (about 35 MB/s at most):
/// one that slow is worth explaining when the link is the reason.
const SLOW_AVERAGE: u64 = 40_000_000;

/// How long a finished transfer took and how fast it went: "Took 1 min
/// 11 s · 45.2 MB/s on average", from the bytes it copied (files already
/// there or skipped cost no time and count for nothing). Nothing for what
/// took under a second. A copy of ten seconds or more over a slow USB link,
/// that went no faster than such a link goes, adds why it was slow.
pub fn timing_line(summary: &dto::TransferSummary, link: Option<dto::UsbSpeed>) -> Option<String> {
    let secs = summary.elapsed.as_secs_f64();
    if secs < 1.0 {
        return None;
    }
    let elapsed = format_elapsed(summary.elapsed.as_secs());
    let rate = (summary.bytes as f64 / secs) as u64;
    let mut line = if summary.bytes > 0 {
        tf(
            "transfer.timing.rate",
            &[("elapsed", &elapsed), ("rate", &format_bytes(rate))],
        )
    } else {
        tf("transfer.timing.took", &[("elapsed", &elapsed)])
    };
    let over_usb = matches!(
        summary.direction,
        dto::Direction::Download | dto::Direction::Upload
    );
    if over_usb
        && summary.bytes > 0
        && secs >= 10.0
        && rate <= SLOW_AVERAGE
        && let Some(slow) = crate::handler::format::slow_link_note(link)
    {
        line = tf(
            "transfer.timing.with_slow",
            &[("line", &line), ("slow", &slow)],
        );
    }
    Some(line)
}

/// A transfer's report in words, one row per file: worded once, when the
/// transfer ends, never per redraw.
pub fn report_rows(report: &[dto::FileReport]) -> Vec<ReportRow> {
    report
        .iter()
        .map(|file| {
            let (mark, mut what) = match &file.outcome {
                dto::FileOutcome::Copied { checked } => (
                    "✓",
                    t(if *checked {
                        key("transfer.report.copied")
                    } else {
                        key("transfer.report.copied_unchecked")
                    })
                    .to_owned(),
                ),
                dto::FileOutcome::Resumed { checked } => (
                    "✓",
                    t(if *checked {
                        key("transfer.report.resumed")
                    } else {
                        key("transfer.report.resumed_unchecked")
                    })
                    .to_owned(),
                ),
                dto::FileOutcome::Sent => ("✓", t("transfer.report.sent").to_owned()),
                dto::FileOutcome::AlreadyThere => {
                    ("=", t("transfer.report.already_there").to_owned())
                }
                dto::FileOutcome::Replaced => ("↻", t("transfer.report.replaced").to_owned()),
                dto::FileOutcome::ReplacedOnPhone => {
                    ("↻", t("transfer.report.replaced_phone").to_owned())
                }
                dto::FileOutcome::Skipped => ("–", t("transfer.report.skipped").to_owned()),
                dto::FileOutcome::Failed(failure) => (
                    "✕",
                    tf(
                        "transfer.report.failed",
                        &[("reason", &error::describe_again(failure).title)],
                    ),
                ),
                dto::FileOutcome::NotCopied => ("✕", t("transfer.report.not_copied").to_owned()),
                dto::FileOutcome::Identical => ("✓", t("transfer.report.identical").to_owned()),
                // Counted from 1 when shown, as `cmp` says it.
                dto::FileOutcome::Differs { at } => (
                    "≠",
                    tf(
                        "transfer.report.differs",
                        &[("at", &format_number(at.saturating_add(1)))],
                    ),
                ),
                dto::FileOutcome::SizesDiffer { phone, mac } => (
                    "≠",
                    tf(
                        "transfer.report.sizes_differ",
                        &[
                            ("phone", &format_bytes(*phone)),
                            ("mac", &format_bytes(*mac)),
                        ],
                    ),
                ),
                dto::FileOutcome::NotOnMac => ("?", t("transfer.report.not_on_mac").to_owned()),
                dto::FileOutcome::NotOnPhone => ("?", t("transfer.report.not_on_phone").to_owned()),
            };
            if let Some(from) = &file.from {
                what = tf("transfer.report.was", &[("what", &what), ("from", from)]);
            }
            let original = match &file.original {
                None => String::new(),
                Some(dto::Original::Removed) => t("transfer.report.original_removed").to_owned(),
                Some(dto::Original::Kept(reason)) => tf(
                    "transfer.report.original_kept_why",
                    &[("why", &why(reason))],
                ),
                Some(dto::Original::Stayed) => t("transfer.report.original_kept").to_owned(),
            };
            ReportRow {
                mark,
                path: file.path.clone(),
                what,
                original,
            }
        })
        .collect()
}

/// Above a report: whose it is and how many files it lists.
pub fn report_title(title: &str, listed: usize, left_out: usize) -> String {
    // A title that has a colon of its own ("Checked 5 files: 2 differ")
    // would read oddly with a second one.
    let has_colon = title.contains([':', '：']);
    let listed_items = items(listed);
    match (has_colon, left_out) {
        (false, 0) => tf(
            "transfer.report.title",
            &[("title", title), ("items", &listed_items)],
        ),
        (true, 0) => tf(
            "transfer.report.title_alt",
            &[("title", title), ("items", &listed_items)],
        ),
        (false, n) => tf(
            "transfer.report.title_part",
            &[
                ("title", title),
                ("items", &listed_items),
                ("count", &format_count(n)),
            ],
        ),
        (true, n) => tf(
            "transfer.report.title_alt_part",
            &[
                ("title", title),
                ("items", &listed_items),
                ("count", &format_count(n)),
            ],
        ),
    }
}

/// After the report was saved: the file's name, and the folder it is in.
pub fn saved_note(path: &str) -> String {
    let path = Path::new(path);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = path
        .parent()
        .map(|folder| folder_name(&folder.to_string_lossy()))
        .unwrap_or_default();
    tf(
        "transfer.report.saved",
        &[("name", &name), ("folder", &folder)],
    )
}

/// After the report was put on the clipboard.
pub fn copied_note(lines: usize) -> String {
    tf("transfer.report.copied_note", &[("items", &items(lines))])
}

/// One cell of the report's text: odd characters written out, and a cell a
/// spreadsheet would take for a formula (a phone can name a file
/// "=HYPERLINK(…)") made plain text with a leading apostrophe.
fn cell(text: &str) -> String {
    let text = escape_odd(text);
    if text.starts_with(['=', '+', '-', '@']) {
        format!("'{text}")
    } else {
        text
    }
}

/// A report as tab-separated text, for a spreadsheet: a header, then one
/// line per file. A name can hold a tab or a line break (the phone's names
/// are anyone's), so odd characters are written out and each file stays on
/// its line.
pub fn report_tsv(rows: &[ReportRow], left_out: usize) -> String {
    let mut text = [
        t("transfer.report.column_file"),
        t("transfer.report.column_what"),
        t("transfer.report.column_original"),
    ]
    .join("\t");
    text.push('\n');
    for row in rows {
        text.push_str(&format!(
            "{}\t{}\t{}\n",
            cell(&row.path),
            cell(&row.what),
            cell(&row.original)
        ));
    }
    if left_out > 0 {
        text.push_str(&count("transfer.report.more_not_listed", left_out, &[]));
        text.push('\n');
    }
    text
}

/// Why an original was not removed, in words for the person who moved it.
fn why(why: &dto::Why) -> String {
    match why {
        dto::Why::Unchecked => t("transfer.why.unchecked").to_owned(),
        dto::Why::Changed => t("transfer.why.changed").to_owned(),
        dto::Why::NotEmpty => t("transfer.why.not_empty").to_owned(),
        dto::Why::Refused(error) => error::describe_again(error).title,
        dto::Why::NotCopied => t("transfer.why.not_copied").to_owned(),
    }
}

/// A byte offset with its thousands marked: "3,145,735".
fn format_number(number: u64) -> String {
    usize::try_from(number).map_or_else(|_| number.to_string(), format_count)
}

fn files(number: usize) -> String {
    count("text.files", number, &[])
}

fn items(number: usize) -> String {
    count("text.items", number, &[])
}

/// One line per failure, "IMG_1.jpg — Your phone stopped responding", the first
/// few, then how many more there were.
fn failure_lines(failures: &[dto::FileFailure]) -> Vec<String> {
    let mut lines: Vec<String> = failures
        .iter()
        .take(MAX_FAILURES_SHOWN)
        .map(|failure| {
            tf(
                "transfer.failure.line",
                &[
                    ("label", &failure.label),
                    ("reason", &error::describe_again(&failure.error).title),
                ],
            )
        })
        .collect();
    if failures.len() > MAX_FAILURES_SHOWN {
        lines.push(count(
            "explorer.paste.more",
            failures.len() - MAX_FAILURES_SHOWN,
            &[],
        ));
    }
    lines
}

/// "Download “IMG_001.jpg” to Downloads", "Upload 3 items to DCIM": one line
/// per queued or finished transfer.
pub fn job_label(
    direction: dto::Direction,
    moving: bool,
    names: &[String],
    destination: &str,
) -> String {
    let folder = folder_name(destination);
    let message = match (direction, moving, names.len() == 1) {
        (_, true, true) => key("transfer.label.move_one"),
        (_, true, false) => key("transfer.label.move_many"),
        (dto::Direction::Download, false, true) => key("transfer.label.download_one"),
        (dto::Direction::Download, false, false) => key("transfer.label.download_many"),
        (dto::Direction::Upload, false, true) => key("transfer.label.upload_one"),
        (dto::Direction::Upload, false, false) => key("transfer.label.upload_many"),
        (dto::Direction::Local, false, true) => key("transfer.label.copy_one"),
        (dto::Direction::Local, false, false) => key("transfer.label.copy_many"),
    };
    match names {
        [one] => {
            let name = Path::new(one)
                .file_name()
                .map_or_else(|| one.clone(), |n| n.to_string_lossy().into_owned());
            tf(message, &[("name", &name), ("folder", &folder)])
        }
        many => tf(
            message,
            &[("items", &items(many.len())), ("folder", &folder)],
        ),
    }
}

/// "Copy “IMG_001.jpg” to Camera via your Mac", "Move 3 items to Camera via your Mac".
pub fn on_phone_label(mode: dto::Paste, names: &[String], destination: &str) -> String {
    let folder = folder_name(destination);
    let message = match (mode, names.len() == 1) {
        (dto::Paste::Copy, true) => key("transfer.label.mac_copy_one"),
        (dto::Paste::Copy, false) => key("transfer.label.mac_copy_many"),
        (dto::Paste::Move, true) => key("transfer.label.mac_move_one"),
        (dto::Paste::Move, false) => key("transfer.label.mac_move_many"),
    };
    match names {
        [one] => tf(message, &[("name", one), ("folder", &folder)]),
        many => tf(
            message,
            &[("items", &items(many.len())), ("folder", &folder)],
        ),
    }
}

/// "“notes.txt” already exists on the phone" or "3 items already exist on the phone".
pub fn conflict_title(existing: &[String], on_mac: bool) -> String {
    match (existing, on_mac) {
        ([one], true) => tf("transfer.conflict.one_mac", &[("name", one)]),
        ([one], false) => tf("transfer.conflict.one_phone", &[("name", one)]),
        (many, true) => tf(
            "transfer.conflict.many_mac",
            &[("count", &many.len().to_string())],
        ),
        (many, false) => tf(
            "transfer.conflict.many_phone",
            &[("count", &many.len().to_string())],
        ),
    }
}

/// The last part of a path, or "the top folder" for a storage root.
fn folder_name(path: &str) -> String {
    match Path::new(path).file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => t("explorer.top_folder").to_owned(),
    }
}

/// How far along, 0.0 to 1.0. More done than the total says (a phone that
/// sent more than it reported) is simply "all of it", never a bar past the end.
fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        1.0
    } else {
        ((done as f64 / total as f64) as f32).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(compared: dto::Compared) -> SummaryResponse {
        to_check_summary(&dto::TransferSummary {
            destination: "/Users/me/Backup".into(),
            compared: Some(compared),
            ..Default::default()
        })
    }

    #[test]
    fn a_check_says_what_it_found_in_counts() {
        let all = |identical| dto::Compared {
            identical,
            ..Default::default()
        };
        assert_eq!(checked(all(1)).title, "Identical");
        assert_eq!(checked(all(5_000)).title, "All 5,000 files identical");
        assert_eq!(checked(all(0)).title, "Nothing to check");
        assert_eq!(
            checked(dto::Compared {
                identical: 0,
                differ: 2,
                missing: 0,
                unreadable: 3,
            })
            .title,
            "Checked 5 files: 2 differ, 3 couldn't be read"
        );
        assert_eq!(
            checked(all(2)).detail,
            "against Backup on your Mac · nothing was changed"
        );
        assert!(!checked(all(2)).mismatched);
        assert!(!is_troubled(&Ok(checked(all(2)))));
        let missing = checked(dto::Compared {
            identical: 1,
            missing: 1,
            ..Default::default()
        });
        assert!(missing.mismatched, "one side only is not the same either");
        assert!(
            is_troubled(&Ok(missing)),
            "the problem sound, not the done one"
        );
        assert_eq!(
            report_title("Checked 2 files: 1 differs, 1 identical", 2, 0),
            "Checked 2 files: 1 differs, 1 identical · 2 items"
        );
        assert_eq!(
            report_title("Downloaded 2 files", 2, 0),
            "Downloaded 2 files: 2 items"
        );
    }

    #[test]
    fn a_checks_report_says_each_file_in_words() {
        let line = |outcome| dto::FileReport {
            path: "a.jpg".into(),
            from: None,
            outcome,
            original: None,
        };
        let rows = report_rows(&[
            line(dto::FileOutcome::Identical),
            line(dto::FileOutcome::Differs { at: 0 }),
            line(dto::FileOutcome::SizesDiffer {
                phone: 5_000_000,
                mac: 12,
            }),
            line(dto::FileOutcome::NotOnMac),
            line(dto::FileOutcome::NotOnPhone),
        ]);
        let said: Vec<(&str, &str)> = rows.iter().map(|r| (r.mark, r.what.as_str())).collect();
        assert_eq!(
            said,
            [
                ("✓", "Identical"),
                ("≠", "Differs at byte 1"),
                ("≠", "Sizes differ: 5.0 MB on the phone, 12 B on the Mac"),
                ("?", "Not on the Mac"),
                ("?", "Not on the phone"),
            ]
        );
    }

    #[test]
    fn a_check_is_named_by_what_it_compares() {
        let request = |names: &[&str]| dto::CheckRequest {
            storage_id: 1,
            phone_folder: "/DCIM".into(),
            mac_folder: "/Users/me/Backup".into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
        };
        assert_eq!(
            check_label(&request(&["IMG_1.jpg"])),
            "Check “IMG_1.jpg” against Backup on your Mac"
        );
        let progress = to_check_progress(&dto::TransferProgress {
            files_total: 2,
            bytes_total: 10,
            ..Default::default()
        });
        assert!(
            progress.title.starts_with("Checking "),
            "{}",
            progress.title
        );
        assert_eq!(progress.note.as_deref(), Some(check_note()));
        assert_eq!(preparing_check().note.as_deref(), Some(check_note()));
    }
    #[test]
    fn the_finished_line_says_how_long_and_how_fast_from_what_was_copied() {
        let took = |secs: u64, bytes: u64| dto::TransferSummary {
            direction: dto::Direction::Download,
            bytes,
            elapsed: Duration::from_secs(secs),
            ..Default::default()
        };
        assert_eq!(timing_line(&took(0, 5_000), None), None, "under a second");
        assert_eq!(
            timing_line(&took(71, 71 * 45_000_000), None).as_deref(),
            Some(
                format!(
                    "Took 1 min 11 s · {}/s on average",
                    format_bytes(45_000_000)
                )
                .as_str()
            )
        );
        assert_eq!(
            timing_line(&took(5, 0), None).as_deref(),
            Some("Took 5 s"),
            "nothing copied (all there already): no speed"
        );

        // A long copy over USB 2 says why it was slow; a short one doesn't,
        // nor a copy inside the Mac.
        let slow = Some(dto::UsbSpeed::High);
        let note = crate::handler::format::slow_link_note(slow).unwrap();
        assert_eq!(
            timing_line(&took(20, 600_000_000), slow).unwrap(),
            format!(
                "Took 20 s · {}/s on average. {note}",
                format_bytes(30_000_000)
            )
        );
        assert!(
            !timing_line(&took(20, 20 * 45_000_000), slow)
                .unwrap()
                .contains(&note),
            "faster than USB 2 goes: the link isn't why"
        );
        assert_eq!(
            timing_line(&took(20, 0), slow).as_deref(),
            Some("Took 20 s"),
            "nothing copied: the link isn't why either"
        );
        assert!(
            timing_line(&took(10, 10 * SLOW_AVERAGE), slow)
                .unwrap()
                .contains(&note)
        );
        assert!(
            !timing_line(&took(9, 300_000_000), slow)
                .unwrap()
                .contains(&note)
        );
        let local = dto::TransferSummary {
            direction: dto::Direction::Local,
            ..took(20, 600_000_000)
        };
        assert!(!timing_line(&local, slow).unwrap().contains(&note));
        assert!(
            !timing_line(&took(20, 600_000_000), Some(dto::UsbSpeed::Super))
                .unwrap()
                .contains("USB")
        );
    }

    fn reported(path: &str, outcome: dto::FileOutcome) -> dto::FileReport {
        dto::FileReport {
            path: path.into(),
            from: None,
            outcome,
            original: None,
        }
    }

    #[test]
    fn each_file_of_a_report_says_what_became_of_it_and_of_its_original() {
        use dto::FileOutcome::*;
        let mut renamed = reported("a (1).jpg", Copied { checked: true });
        renamed.from = Some("a.jpg".into());
        renamed.original = Some(dto::Original::Removed);
        let mut kept = reported("b.jpg", Copied { checked: false });
        kept.original = Some(dto::Original::Kept(dto::Why::Changed));
        let mut stayed = reported("c.jpg", Failed(dto::Error::MacDiskFull));
        stayed.original = Some(dto::Original::Stayed);
        let report = [
            renamed,
            kept,
            stayed,
            reported("d.jpg", Resumed { checked: true }),
            reported("e.jpg", AlreadyThere),
            reported("f.jpg", Replaced),
            reported("Trip", Skipped),
            reported("g.jpg", NotCopied),
            reported("h.jpg", Sent),
            reported("i.jpg", ReplacedOnPhone),
        ];

        let rows = report_rows(&report);

        let line = |at: usize| {
            (
                rows[at].mark,
                rows[at].what.clone(),
                rows[at].original.clone(),
            )
        };
        let full = format!(
            "Failed: {}",
            error::to_error_response(dto::Error::MacDiskFull).title
        );
        assert_eq!(rows[0].path, "a (1).jpg");
        assert_eq!(
            line(0),
            (
                "✓",
                "Copied and checked · was “a.jpg”".into(),
                "Original removed".into()
            )
        );
        assert_eq!(
            line(1),
            (
                "✓",
                "Copied; the phone gave no size to check it by".into(),
                "Original kept: it changed after it was copied".into()
            )
        );
        assert_eq!(line(2), ("✕", full, "Original kept".into()));
        assert_eq!(
            line(3),
            (
                "✓",
                "Finished from an earlier try and checked".into(),
                String::new()
            )
        );
        assert_eq!(line(4), ("=", "Already there".into(), String::new()));
        assert_eq!(
            line(5),
            (
                "↻",
                "Replaced; the old one is in the Trash".into(),
                String::new()
            )
        );
        assert_eq!(
            line(6),
            ("–", "Skipped: the name was taken".into(), String::new())
        );
        assert_eq!(line(7), ("✕", "Not copied".into(), String::new()));
        assert_eq!(
            line(8),
            ("✓", "Sent; the phone took it".into(), String::new())
        );
        assert_eq!(
            line(9),
            (
                "↻",
                "Replaced; the phone deleted its old one".into(),
                String::new()
            ),
            "a phone has no Trash"
        );
    }

    #[test]
    fn a_report_as_text_is_one_line_per_file_whatever_the_names_hold() {
        let rows = report_rows(&[
            reported("a.jpg", dto::FileOutcome::AlreadyThere),
            reported("evil\tname\nwith lines.jpg", dto::FileOutcome::AlreadyThere),
        ]);

        let text = report_tsv(&rows, 0);

        assert_eq!(
            text,
            "File\tWhat happened\tOriginal\n\
             a.jpg\tAlready there\t\n\
             evil\\tname\\nwith lines.jpg\tAlready there\t\n"
        );
        assert_eq!(text.lines().count(), 3, "a header and one line each");

        // A name a spreadsheet would run as a formula stays text.
        for name in ["=HYPERLINK(\"x\")", "+1", "-2.jpg", "@SUM(A1)"] {
            let rows = report_rows(&[reported(name, dto::FileOutcome::AlreadyThere)]);
            let line = report_tsv(&rows, 0).lines().nth(1).unwrap().to_owned();
            assert!(line.starts_with(&format!("'{name}\t")), "{line}");
        }
        let plain = report_rows(&[reported("a=b.jpg", dto::FileOutcome::AlreadyThere)]);
        assert!(
            report_tsv(&plain, 0).contains("\na=b.jpg\t"),
            "only a leading one"
        );
        assert!(report_tsv(&rows, 1_200).ends_with("(1,200 more not listed)\n"));
    }

    #[test]
    fn the_reports_title_and_notes_say_how_many_and_where() {
        assert_eq!(
            report_title("Downloaded 3 files", 3, 0),
            "Downloaded 3 files: 3 items"
        );
        assert_eq!(
            report_title("Downloaded", 100_000, 6),
            "Downloaded: the first 100,000 items (6 more not listed)"
        );
        assert_eq!(
            saved_note("/Users/me/Photos/Transfer report 2026-10-17 14.32.tsv"),
            "Saved “Transfer report 2026-10-17 14.32.tsv” in “Photos”"
        );
        assert_eq!(
            copied_note(1),
            "Copied the report (1 item): paste it into a spreadsheet or a note"
        );
    }

    #[test]
    fn only_a_report_of_files_on_the_mac_can_be_saved_there() {
        let report = vec![reported("a.jpg", dto::FileOutcome::AlreadyThere)];
        let download = dto::TransferSummary {
            direction: dto::Direction::Download,
            destination: "/Users/me/Photos".into(),
            report: report.clone(),
            report_left_out: 2,
            ..Default::default()
        };
        let response = to_summary_response(&download);
        assert_eq!(response.report_folder.as_deref(), Some("/Users/me/Photos"));
        assert_eq!((response.report.len(), response.report_left_out), (1, 2));

        let upload = dto::TransferSummary {
            direction: dto::Direction::Upload,
            ..download.clone()
        };
        assert_eq!(to_summary_response(&upload).report_folder, None);
        let through = dto::TransferSummary {
            through_mac: true,
            ..download.clone()
        };
        assert_eq!(to_summary_response(&through).report_folder, None);
        let empty = dto::TransferSummary {
            report: Vec::new(),
            ..download
        };
        assert_eq!(to_summary_response(&empty).report_folder, None);
        assert!(to_import_summary(&empty).report.is_empty());
    }

    use std::time::Duration;

    fn progress(bytes_done: u64, elapsed_secs: u64) -> dto::TransferProgress {
        dto::TransferProgress {
            current_file: "DCIM/a.jpg".into(),
            files_done: 1,
            files_total: 3,
            file_bytes_done: 500_000,
            file_bytes_total: 1_000_000,
            bytes_done,
            bytes_total: 10_000_000,
            elapsed: Duration::from_secs(elapsed_secs),
            ..Default::default()
        }
    }

    #[test]
    fn preparing_has_no_progress_yet() {
        let response = preparing_response(dto::Direction::Download, false);

        assert_eq!(response.title, "Preparing download…");
        assert_eq!(
            preparing_response(dto::Direction::Upload, false).title,
            "Preparing upload…"
        );
        assert_eq!(response.total_fraction, 0.0);
    }

    #[test]
    fn progress_names_the_file_being_copied_and_both_fractions() {
        let response = to_progress_response(&progress(2_000_000, 2));

        assert_eq!(response.title, "Downloading 2 of 3");
        assert_eq!(response.current_file, "DCIM/a.jpg");
        assert!((response.file_fraction - 0.5).abs() < 1e-6);
        assert!((response.total_fraction - 0.2).abs() < 1e-6);
    }

    #[test]
    fn progress_shows_speed_and_time_left_once_measurable() {
        let response = to_progress_response(&progress(2_000_000, 2));

        assert_eq!(
            response.detail,
            "2.0 MB of 10.0 MB · 1.0 MB/s · about 8 s left"
        );
    }

    #[test]
    fn progress_waits_before_estimating() {
        let response = to_progress_response(&progress(2_000_000, 0));

        assert_eq!(response.detail, "2.0 MB of 10.0 MB · estimating time…");
    }

    #[test]
    fn the_count_never_passes_the_total() {
        let done = dto::TransferProgress {
            files_done: 3,
            files_total: 3,
            ..progress(10_000_000, 5)
        };

        assert_eq!(to_progress_response(&done).title, "Downloading 3 of 3");
    }

    #[test]
    fn empty_files_count_as_complete() {
        let empty = dto::TransferProgress {
            file_bytes_total: 0,
            bytes_total: 0,
            ..progress(0, 0)
        };

        let response = to_progress_response(&empty);

        assert_eq!(
            (response.file_fraction, response.total_fraction),
            (1.0, 1.0)
        );
    }

    fn summary(files: usize, renamed: usize) -> dto::TransferSummary {
        dto::TransferSummary {
            files,
            bytes: 48_000_000,
            renamed,
            destination: "/Users/me/Downloads".into(),
            first_item: Some("DCIM".into()),
            ..Default::default()
        }
    }

    fn counted(files: usize, there: usize, checked: Option<usize>, complete: bool) -> dto::Arrival {
        dto::Arrival {
            files,
            bytes: 3_200_000_000,
            there,
            checked,
            complete,
        }
    }

    #[test]
    fn the_count_at_the_end_says_all_arrived_only_when_every_file_was_checked() {
        let line = |arrival| arrival_line(&arrival).unwrap();

        assert_eq!(
            line(counted(142, 142, Some(142), true)),
            "142 files, 3.2 GB — all arrived"
        );
        assert_eq!(
            line(counted(142, 142, None, true)),
            "142 files, 3.2 GB — all sent, and the phone took each one"
        );
        assert_eq!(
            line(counted(142, 142, Some(140), true)),
            "142 files, 3.2 GB — all there; 2 could not be checked (no size to compare)"
        );
        assert_eq!(
            line(counted(142, 141, Some(141), true)),
            "141 of 142 files arrived"
        );
        assert_eq!(line(counted(1, 0, Some(0), true)), "0 of 1 file arrived");
        assert_eq!(
            line(counted(1_500, 1_500, Some(1_500), false)),
            "1,500 files, 3.2 GB — all that could be seen; a folder listed inside itself was not gone into"
        );
        assert_eq!(
            arrival_line(&counted(0, 0, Some(0), true)),
            None,
            "an empty folder"
        );
    }

    #[test]
    fn a_background_note_says_one_transfer_in_its_words_and_counts_several() {
        let clean = SummaryResponse {
            arrival: Some("2 files, 3.2 GB — all arrived".into()),
            ..to_summary_response(&summary(2, 0))
        };
        let failed_one = SummaryResponse {
            failures: vec!["a.jpg — refused".into()],
            ..to_summary_response(&summary(1, 0))
        };
        let lost = Err(error::ErrorResponse {
            title: "The phone was disconnected.".into(),
            hint: String::new(),
            device_lost: true,
            privacy: None,
            cancelled: false,
            source: dto::Error::Disconnected,
        });
        let label = |n: &str| n.to_owned();

        assert_eq!(batch_note(&[]), None);
        assert_eq!(
            batch_note(&[(label("Download"), &Ok(clean.clone()))]),
            Some((
                "Downloaded 2 files (48.0 MB)".to_owned(),
                "2 files, 3.2 GB — all arrived".to_owned()
            ))
        );
        assert_eq!(
            batch_note(&[(label("Download"), &Ok(failed_one.clone()))])
                .unwrap()
                .1,
            failed_one.detail,
            "with a failure the detail, which says so, beats the count"
        );
        assert_eq!(
            batch_note(&[(label("Download a.jpg"), &lost)]),
            Some((
                "Download a.jpg: stopped".to_owned(),
                "The phone was disconnected.".to_owned()
            ))
        );
        let several = batch_note(&[
            (label("A"), &Ok(clean.clone())),
            (label("B"), &lost),
            (label("C"), &Ok(failed_one)),
            (label("D"), &Ok(clean)),
        ])
        .unwrap();
        assert_eq!(several.0, "4 transfers finished, 2 with problems");
        assert_eq!(
            several.1, "Downloaded 2 files (48.0 MB) · B: stopped · Downloaded 1 file (48.0 MB)",
            "the first three"
        );
        assert_eq!(
            question_note(1).1,
            "A name is already taken on the phone: choose what to do with it."
        );
    }

    #[test]
    fn the_count_reaches_every_summary_that_has_one() {
        let with = dto::TransferSummary {
            arrival: Some(counted(2, 2, Some(2), true)),
            ..summary(2, 0)
        };
        assert_eq!(
            to_summary_response(&with).arrival.as_deref(),
            Some("2 files, 3.2 GB — all arrived")
        );
        assert!(to_import_summary(&with).arrival.is_some());
        assert_eq!(to_summary_response(&summary(2, 0)).arrival, None);
    }

    #[test]
    fn summary_says_what_went_where() {
        let response = to_summary_response(&summary(12, 0));

        assert_eq!(response.title, "Downloaded 12 files (48.0 MB)");
        assert_eq!(response.detail, "to Downloads");
        assert_eq!(response.reveal.as_deref(), Some("/Users/me/Downloads/DCIM"));
    }

    #[test]
    fn summary_explains_renamed_items() {
        assert_eq!(
            to_summary_response(&summary(1, 1)).detail,
            "to Downloads · 1 saved under a new name because the name was taken"
        );
        assert_eq!(
            to_summary_response(&summary(3, 2)).detail,
            "to Downloads · 2 saved under new names because the names were taken"
        );
        assert_eq!(
            to_summary_response(&summary(1, 0)).title,
            "Downloaded 1 file (48.0 MB)"
        );
    }

    #[test]
    fn summary_without_items_has_nothing_to_reveal() {
        let empty = dto::TransferSummary {
            first_item: None,
            destination: "/".into(),
            ..summary(0, 0)
        };

        let response = to_summary_response(&empty);

        assert_eq!(response.reveal, None);
        assert_eq!(response.detail, "to the top folder");
    }

    #[test]
    fn uploads_say_uploading_and_where_they_went() {
        let up = dto::TransferProgress {
            direction: dto::Direction::Upload,
            ..progress(2_000_000, 2)
        };
        let done = dto::TransferSummary {
            direction: dto::Direction::Upload,
            destination: "/DCIM".into(),
            skipped: 1,
            replaced: 2,
            ..summary(3, 0)
        };

        assert_eq!(to_progress_response(&up).title, "Uploading 2 of 3");
        let response = to_summary_response(&done);
        assert_eq!(response.title, "Uploaded 3 files (48.0 MB)");
        assert_eq!(response.detail, "to DCIM · 1 skipped · 2 replaced");
        assert_eq!(response.reveal, None);
    }

    #[test]
    fn conflict_title_names_one_item_and_counts_many() {
        assert_eq!(
            conflict_title(&["notes.txt".to_owned()], false),
            "“notes.txt” already exists on the phone"
        );
        assert_eq!(
            conflict_title(&["a".to_owned(), "b".to_owned()], false),
            "2 items already exist on the phone"
        );
        assert_eq!(
            conflict_title(&["DCIM".to_owned()], true),
            "“DCIM” already exists on your Mac"
        );
    }

    #[test]
    fn job_labels_name_one_item_or_count_many() {
        assert_eq!(
            job_label(
                dto::Direction::Download,
                false,
                &["IMG_001.jpg".into()],
                "/Users/me/Downloads"
            ),
            "Download “IMG_001.jpg” to Downloads"
        );
        assert_eq!(
            job_label(
                dto::Direction::Download,
                true,
                &["IMG_001.jpg".into()],
                "/Users/me/Downloads"
            ),
            "Move “IMG_001.jpg” to Downloads"
        );
        assert_eq!(
            job_label(
                dto::Direction::Upload,
                false,
                &["/Users/me/a.jpg".into(), "/Users/me/b.jpg".into()],
                "/"
            ),
            "Upload 2 items to the top folder"
        );
        assert_eq!(
            job_label(
                dto::Direction::Upload,
                false,
                &["/Users/me/a.jpg".into()],
                "/DCIM"
            ),
            "Upload “a.jpg” to DCIM"
        );
    }

    #[test]
    fn summary_says_what_was_already_there_and_what_was_continued() {
        let response = to_summary_response(&dto::TransferSummary {
            already_there: 4,
            resumed: 1,
            ..summary(2, 0)
        });

        assert_eq!(response.title, "Downloaded 2 files (48.0 MB)");
        assert_eq!(
            response.detail,
            "to Downloads · 4 already there · 1 continued from an earlier attempt"
        );
        let many = to_summary_response(&dto::TransferSummary {
            resumed: 3,
            ..summary(3, 0)
        });
        assert_eq!(
            many.detail,
            "to Downloads · 3 continued from an earlier attempt"
        );
    }

    #[test]
    fn a_retry_that_finds_everything_in_place_says_so() {
        let response = to_summary_response(&dto::TransferSummary {
            already_there: 12,
            bytes: 0,
            ..summary(0, 0)
        });

        assert_eq!(response.title, "Nothing new to copy");
        assert_eq!(response.detail, "to Downloads · 12 already there");
        assert_eq!(
            response.reveal.as_deref(),
            Some("/Users/me/Downloads/DCIM"),
            "what is there can still be shown"
        );
    }

    #[test]
    fn an_empty_copy_with_nothing_skipped_is_still_reported_as_a_copy() {
        let response = to_summary_response(&summary(0, 0));

        assert_eq!(response.title, "Downloaded 0 files (48.0 MB)");
    }

    fn failed(label: &str, error: dto::Error) -> dto::FileFailure {
        dto::FileFailure {
            label: label.into(),
            error,
        }
    }

    #[test]
    fn the_last_moments_of_a_file_say_finishing() {
        let settling = dto::TransferProgress {
            finishing: true,
            ..progress(1_000_000, 1)
        };

        assert_eq!(
            to_progress_response(&settling).current_file,
            "Finishing DCIM/a.jpg…"
        );
        assert_eq!(
            to_progress_response(&progress(1_000_000, 1)).current_file,
            "DCIM/a.jpg",
            "not while the bytes are still coming"
        );
    }

    #[test]
    fn the_note_says_the_mac_stays_awake_only_when_it_does() {
        let note = "Keep your phone unlocked and plugged in until this finishes.";
        assert_eq!(
            with_awake(note, true),
            "Keep your phone unlocked and plugged in until this finishes. Your Mac will stay awake until then."
        );
        assert_eq!(with_awake(note, false), note);
        assert_eq!(
            with_link(note.to_owned(), Some(dto::UsbSpeed::High)),
            format!(
                "{note} Connected at USB 2 speed (480 Mb/s): about 35 MB/s at most. A USB 3 cable and port would be faster."
            )
        );
        assert_eq!(with_link(note.to_owned(), Some(dto::UsbSpeed::Super)), note);
        assert_eq!(with_link(note.to_owned(), None), note);
    }

    #[test]
    fn a_long_copy_asks_for_the_phone_to_stay_unlocked() {
        let long = dto::TransferProgress {
            bytes_total: 100_000_000,
            ..progress(1_000_000, 1)
        };
        let short = dto::TransferProgress {
            bytes_total: 99_999_999,
            ..progress(1_000_000, 1)
        };

        assert_eq!(
            to_progress_response(&long).note.as_deref(),
            Some("Keep your phone unlocked and plugged in until this finishes.")
        );
        assert_eq!(to_progress_response(&short).note, None);
        assert_eq!(
            preparing_response(dto::Direction::Download, false).note,
            None
        );
    }

    #[test]
    fn a_result_with_failures_names_them_and_counts_them() {
        let response = to_summary_response(&dto::TransferSummary {
            failures: vec![
                failed("DCIM/IMG_1.jpg", dto::Error::AllowStorageAccess),
                failed("DCIM/IMG_2.jpg", dto::Error::MacDiskFull),
            ],
            ..summary(10, 0)
        });

        assert_eq!(response.title, "Downloaded 10 files (48.0 MB)");
        assert_eq!(response.detail, "to Downloads · 2 failed");
        assert_eq!(
            response.failures,
            [
                "DCIM/IMG_1.jpg — Allow access on your phone",
                "DCIM/IMG_2.jpg — Your Mac's disk is full"
            ]
        );
    }

    #[test]
    fn only_the_first_failures_are_spelled_out() {
        let many: Vec<_> = (0..8)
            .map(|i| failed(&format!("IMG_{i}.jpg"), dto::Error::Unsupported))
            .collect();

        let response = to_summary_response(&dto::TransferSummary {
            failures: many,
            ..summary(3, 0)
        });

        assert_eq!(response.failures.len(), 6);
        assert_eq!(
            response.failures[4],
            "IMG_4.jpg — Your phone can't do this over USB"
        );
        assert_eq!(response.failures[5], "and 3 more");
        assert!(response.detail.ends_with("8 failed"), "{}", response.detail);
    }

    #[test]
    fn a_result_where_nothing_arrived_says_so_not_nothing_new() {
        let response = to_summary_response(&dto::TransferSummary {
            already_there: 3,
            failures: vec![failed("a.jpg", dto::Error::Unsupported)],
            ..summary(0, 0)
        });

        assert_eq!(response.title, "Nothing was copied");
    }

    #[test]
    fn a_clean_result_lists_no_failures() {
        assert!(to_summary_response(&summary(3, 0)).failures.is_empty());
    }

    #[test]
    fn progress_past_4_gb_shows_gigabytes_speed_and_time_left() {
        let progress = dto::TransferProgress {
            current_file: "big.mkv".into(),
            files_done: 0,
            files_total: 1,
            file_bytes_done: 4_200_000_000,
            file_bytes_total: 5_028_970_496,
            bytes_done: 4_200_000_000,
            bytes_total: 5_028_970_496,
            elapsed: Duration::from_secs(100),
            ..dto::TransferProgress::default()
        };

        let response = to_progress_response(&progress);

        assert_eq!(response.title, "Downloading 1 of 1");
        assert_eq!(
            response.detail,
            "4.2 GB of 5.0 GB · 42.0 MB/s · about 20 s left"
        );
        assert!(
            (response.file_fraction - 0.835).abs() < 0.001,
            "{}",
            response.file_fraction
        );
        assert!((response.total_fraction - 0.835).abs() < 0.001);
        assert!(
            response.note.is_some(),
            "a copy this long asks for the phone to stay unlocked"
        );
    }

    #[test]
    fn a_fraction_never_runs_past_the_end() {
        assert_eq!(fraction(5_000_000_000, 4_294_967_295), 1.0);
        assert_eq!(fraction(10, 10), 1.0);
        assert_eq!(fraction(0, 10), 0.0);
        assert_eq!(fraction(0, 0), 1.0);
        assert!((fraction(2_514_485_248, 5_028_970_496) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_result_of_a_file_over_4_gb_says_how_big_it_was() {
        let response = to_summary_response(&dto::TransferSummary {
            files: 1,
            bytes: 5_028_970_496,
            destination: "/Users/me/Movies".into(),
            first_item: Some("big.mkv".into()),
            ..Default::default()
        });

        assert_eq!(response.title, "Downloaded 1 file (5.0 GB)");
    }

    // ---- moves ----

    fn moved(files: usize, removal: dto::Removal) -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Download,
            files,
            bytes: 2_500_000,
            destination: "/Users/me/Downloads".into(),
            first_item: Some("a.jpg".into()),
            removal: Some(removal),
            ..Default::default()
        }
    }

    fn kept(label: &str, why: dto::Why) -> dto::Kept {
        dto::Kept {
            label: label.into(),
            why,
        }
    }

    #[test]
    fn a_move_that_left_nothing_behind_says_moved() {
        let response = to_summary_response(&moved(
            12,
            dto::Removal {
                removed: 12,
                ..Default::default()
            },
        ));

        assert_eq!(response.title, "Moved 12 files (2.5 MB)");
        assert_eq!(response.detail, "to Downloads");
        assert!(response.notes.is_empty() && response.left.is_empty());
    }

    #[test]
    fn a_move_that_found_everything_already_copied_says_so() {
        let mut summary = moved(
            0,
            dto::Removal {
                removed: 3,
                ..Default::default()
            },
        );
        summary.already_there = 3;

        let response = to_summary_response(&summary);

        assert_eq!(response.title, "Moved 3 files (already copied)");
        assert!(response.detail.contains("3 already there"));
    }

    #[test]
    fn a_move_that_left_something_says_copied_and_what_is_left() {
        let response = to_summary_response(&moved(
            10,
            dto::Removal {
                removed: 8,
                kept: vec![
                    kept("DCIM/a.jpg", dto::Why::Changed),
                    kept("DCIM/b.mov", dto::Why::Unchecked),
                    kept("DCIM", dto::Why::NotEmpty),
                    kept("c.jpg", dto::Why::Refused(dto::Error::ReadOnly)),
                ],
                left: vec!["DCIM".into(), "c.jpg".into()],
                ..Default::default()
            },
        ));

        assert_eq!(response.title, "Copied 10 files (2.5 MB)");
        assert_eq!(
            response.detail,
            "to Downloads · 8 files removed from the phone · 2 items left on the phone"
        );
        let refusal = error::to_error_response(dto::Error::ReadOnly).title;
        assert_eq!(
            response.notes,
            vec![
                "DCIM/a.jpg — stayed on the phone: it changed after it was copied".to_owned(),
                "DCIM/b.mov — stayed on the phone: its copy couldn't be checked".to_owned(),
                "DCIM — stayed on the phone: it still holds other files".to_owned(),
                format!("c.jpg — stayed on the phone: {refusal}"),
            ]
        );
        assert_eq!(response.left, ["DCIM", "c.jpg"]);
    }

    #[test]
    fn a_long_list_of_originals_that_stayed_is_cut_short_and_counted() {
        let many: Vec<_> = (0..8)
            .map(|i| kept(&format!("f{i}"), dto::Why::Changed))
            .collect();

        let response = to_summary_response(&moved(
            8,
            dto::Removal {
                kept: many,
                left: vec!["x".into()],
                ..Default::default()
            },
        ));

        assert_eq!(response.notes.len(), MAX_FAILURES_SHOWN + 1);
        assert_eq!(
            response.notes.last().unwrap(),
            "and 3 more stayed on the phone"
        );
    }

    #[test]
    fn a_removal_that_stopped_says_how() {
        let cancelled = to_summary_response(&moved(
            2,
            dto::Removal {
                removed: 1,
                left: vec!["a".into()],
                stopped: Some(dto::Error::Cancelled),
                ..Default::default()
            },
        ));
        assert_eq!(
            cancelled.notes,
            ["Removing stopped when you cancelled; the rest stayed on the phone."]
        );

        let lost = to_summary_response(&moved(
            2,
            dto::Removal {
                left: vec!["a".into(), "b".into()],
                stopped: Some(dto::Error::Disconnected),
                ..Default::default()
            },
        ));
        assert_eq!(
            lost.notes,
            [format!(
                "Removing stopped: {}. The rest stayed on the phone.",
                error::to_error_response(dto::Error::Disconnected).title
            )]
        );
    }

    // ---- a move to the phone: the originals are on the Mac, in the Trash ----

    fn moved_up(files: usize, removal: dto::Removal) -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Upload,
            destination: "/DCIM/Camera".into(),
            removal: Some(dto::Removal {
                from_mac: true,
                ..removal
            }),
            ..moved(files, dto::Removal::default())
        }
    }

    #[test]
    fn a_retry_that_merged_into_a_folder_says_so() {
        let mut one = moved_up(
            2,
            dto::Removal {
                removed: 3,
                ..Default::default()
            },
        );
        one.merged = 1;
        one.already_there = 1;
        let mut two = one.clone();
        two.merged = 2;

        assert_eq!(
            to_summary_response(&one).detail,
            "to Camera · merged into the folder already there · 1 already there"
        );
        assert_eq!(
            to_summary_response(&two).detail,
            "to Camera · 2 folders merged into the ones already there · 1 already there"
        );
        assert!(
            !to_summary_response(&moved_up(2, dto::Removal::default()))
                .detail
                .contains("merged")
        );
    }

    #[test]
    fn a_move_to_the_phone_that_left_nothing_behind_says_moved() {
        let response = to_summary_response(&moved_up(
            12,
            dto::Removal {
                removed: 12,
                ..Default::default()
            },
        ));

        assert_eq!(response.title, "Moved 12 files (2.5 MB)");
        assert_eq!(response.detail, "to Camera");
        assert!(response.notes.is_empty() && response.left.is_empty());
        assert_eq!(response.reveal, None, "Finder shows what is on the Mac");
    }

    #[test]
    fn a_move_to_the_phone_that_left_something_names_the_mac_and_the_trash() {
        let response = to_summary_response(&moved_up(
            10,
            dto::Removal {
                removed: 8,
                kept: vec![
                    kept("Trip", dto::Why::NotEmpty),
                    kept("clip.mov", dto::Why::Unchecked),
                    kept("Link", dto::Why::NotCopied),
                ],
                left: vec!["/Users/me/Trip".into(), "/Users/me/clip.mov".into()],
                ..Default::default()
            },
        ));

        assert_eq!(response.title, "Copied 10 files (2.5 MB)");
        assert_eq!(
            response.detail,
            "to Camera · 8 files moved to the Trash · 2 items left on your Mac"
        );
        assert_eq!(
            response.notes,
            [
                "Trip — stayed on your Mac: it still holds other files",
                "clip.mov — stayed on your Mac: its copy couldn't be checked",
                "Link — stayed on your Mac: it can't be copied",
            ]
        );
        assert_eq!(response.left, ["/Users/me/Trip", "/Users/me/clip.mov"]);
    }

    #[test]
    fn a_long_list_of_originals_that_stayed_on_the_mac_is_counted_there() {
        let many: Vec<_> = (0..8)
            .map(|i| kept(&format!("f{i}"), dto::Why::Changed))
            .collect();

        let response = to_summary_response(&moved_up(
            8,
            dto::Removal {
                kept: many,
                left: vec!["x".into()],
                ..Default::default()
            },
        ));

        assert_eq!(response.notes.len(), MAX_FAILURES_SHOWN + 1);
        assert_eq!(
            response.notes.last().unwrap(),
            &format!("and {} more stayed on your Mac", 8 - MAX_FAILURES_SHOWN)
        );
    }

    #[test]
    fn a_trash_that_refused_is_named_in_the_error_s_words() {
        let response = to_summary_response(&moved_up(
            1,
            dto::Removal {
                kept: vec![kept("a.txt", dto::Why::Refused(dto::Error::MacDiskFull))],
                left: vec!["/Users/me/a.txt".into()],
                ..Default::default()
            },
        ));

        assert_eq!(
            response.notes,
            [format!(
                "a.txt — stayed on your Mac: {}",
                error::to_error_response(dto::Error::MacDiskFull).title
            )]
        );
    }

    #[test]
    fn a_move_to_the_phone_that_stopped_says_the_rest_stayed_on_the_mac() {
        let cancelled = to_summary_response(&moved_up(
            2,
            dto::Removal {
                removed: 1,
                left: vec!["a".into()],
                stopped: Some(dto::Error::Cancelled),
                ..Default::default()
            },
        ));
        assert_eq!(
            cancelled.notes,
            ["Removing stopped when you cancelled; the rest stayed on your Mac."]
        );

        let lost = to_summary_response(&moved_up(
            2,
            dto::Removal {
                left: vec!["a".into()],
                stopped: Some(dto::Error::Disconnected),
                ..Default::default()
            },
        ));
        assert_eq!(
            lost.notes,
            [format!(
                "Removing stopped: {}. The rest stayed on your Mac.",
                error::to_error_response(dto::Error::Disconnected).title
            )]
        );
    }

    #[test]
    fn a_retry_that_found_everything_on_the_phone_says_it_was_already_copied() {
        let mut summary = moved_up(
            0,
            dto::Removal {
                removed: 3,
                ..Default::default()
            },
        );
        summary.already_there = 3;

        assert_eq!(
            to_summary_response(&summary).title,
            "Moved 3 files (already copied)"
        );
    }

    #[test]
    fn moving_nothing_but_an_empty_folder_does_not_claim_it_was_already_copied() {
        let response = to_summary_response(&moved_up(0, dto::Removal::default()));

        assert!(
            !response.title.contains("already copied"),
            "{}",
            response.title
        );
    }

    // ---- inside the Mac ----

    fn local(files: usize, removal: Option<dto::Removal>) -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Local,
            files,
            bytes: 2_500_000,
            destination: "/Users/me/Backup".into(),
            first_item: Some("Trip".into()),
            removal,
            ..Default::default()
        }
    }

    #[test]
    fn a_copy_inside_the_mac_says_copied_and_can_be_shown_in_finder() {
        let response = to_summary_response(&local(12, None));

        assert_eq!(response.title, "Copied 12 files (2.5 MB)");
        assert_eq!(response.detail, "to Backup");
        assert_eq!(
            response.reveal.as_deref(),
            Some("/Users/me/Backup/Trip"),
            "what landed is on the Mac"
        );
    }

    #[test]
    fn a_move_to_another_volume_names_the_trash_and_what_stayed_on_the_mac() {
        let clean = to_summary_response(&local(
            3,
            Some(dto::Removal {
                from_mac: true,
                removed: 3,
                ..Default::default()
            }),
        ));
        let partly = to_summary_response(&local(
            3,
            Some(dto::Removal {
                from_mac: true,
                removed: 1,
                kept: vec![kept("Trip", dto::Why::NotCopied)],
                left: vec!["Trip".into()],
                ..Default::default()
            }),
        ));

        assert_eq!(clean.title, "Moved 3 files (2.5 MB)");
        assert_eq!(
            partly.detail,
            "to Backup · 1 file moved to the Trash · 1 item left on your Mac"
        );
        assert_eq!(
            partly.notes,
            ["Trip — stayed on your Mac: it can't be copied"]
        );
    }

    #[test]
    fn progress_inside_the_mac_says_copying_or_moving() {
        let copying = dto::TransferProgress {
            direction: dto::Direction::Local,
            files_done: 1,
            files_total: 3,
            ..Default::default()
        };

        assert_eq!(to_progress_response(&copying).title, "Copying 2 of 3");
        assert_eq!(
            to_progress_response(&dto::TransferProgress {
                moving: true,
                ..copying
            })
            .title,
            "Moving 2 of 3"
        );
    }

    #[test]
    fn trashing_the_originals_of_a_mac_move_does_not_say_the_phone_read_them_back() {
        let trashing = dto::TransferProgress {
            direction: dto::Direction::Local,
            moving: true,
            removing: true,
            trashing: true,
            files_done: 1,
            files_total: 4,
            ..Default::default()
        };

        let response = to_progress_response(&trashing);

        assert_eq!(response.current_file, "1 of 4 items moved to the Trash");
        assert_eq!(
            response.detail,
            "Each one goes only after its copy has been checked."
        );
    }

    #[test]
    fn a_job_label_inside_the_mac_names_the_destination() {
        assert_eq!(
            job_label(
                dto::Direction::Local,
                false,
                &["a.jpg".to_owned()],
                "/Users/me/Backup"
            ),
            "Copy “a.jpg” to Backup"
        );
        assert_eq!(
            job_label(
                dto::Direction::Local,
                true,
                &["a.jpg".to_owned(), "b.jpg".to_owned()],
                "/Users/me/Backup"
            ),
            "Move 2 items to Backup"
        );
    }

    #[test]
    fn trashing_the_originals_counts_items_not_files() {
        let trashing = dto::TransferProgress {
            direction: dto::Direction::Upload,
            moving: true,
            removing: true,
            trashing: true,
            files_done: 2,
            files_total: 8,
            ..Default::default()
        };

        let response = to_progress_response(&trashing);

        assert_eq!(response.title, "Moving the originals to the Trash");
        assert_eq!(response.current_file, "2 of 8 items moved to the Trash");
        assert!((response.total_fraction - 0.25).abs() < 1e-6);
        assert!(response.detail.contains("read back from the phone"));

        let one = dto::TransferProgress {
            files_done: 0,
            files_total: 1,
            ..trashing
        };
        assert_eq!(
            to_progress_response(&one).current_file,
            "0 of 1 item moved to the Trash"
        );
    }

    #[test]
    fn big_counts_in_the_title_have_separators_like_the_rest() {
        let thousands = dto::TransferProgress {
            files_done: 2_199,
            files_total: 5_000,
            ..Default::default()
        };

        assert_eq!(
            to_progress_response(&thousands).title,
            "Downloading 2,200 of 5,000"
        );
        assert_eq!(
            to_progress_response(&dto::TransferProgress {
                through_mac: true,
                direction: dto::Direction::Upload,
                ..thousands
            })
            .title,
            "Copying via your Mac, sending 2,200 of 5,000"
        );
    }

    #[test]
    fn removing_from_the_phone_still_says_so() {
        let removing = dto::TransferProgress {
            removing: true,
            files_done: 1,
            files_total: 2,
            ..Default::default()
        };

        assert_eq!(
            to_progress_response(&removing).title,
            "Removing the originals from the phone"
        );
    }

    #[test]
    fn a_copy_has_no_notes_and_nothing_to_retry_but_its_failures() {
        let response = to_summary_response(&summary(3, 0));

        assert!(response.notes.is_empty());
        assert!(response.left.is_empty());
        assert!(response.title.starts_with("Downloaded"));
    }

    #[test]
    fn a_move_that_copied_nothing_says_so_whatever_it_left() {
        let mut summary = moved(
            0,
            dto::Removal {
                left: vec!["a".into()],
                ..Default::default()
            },
        );
        summary.failures = vec![dto::FileFailure {
            label: "a".into(),
            error: dto::Error::ReadOnly,
        }];

        assert_eq!(to_summary_response(&summary).title, "Nothing was copied");
    }

    #[test]
    fn progress_and_labels_say_moving() {
        let mut moving = progress(1_000_000, 1);
        moving.moving = true;

        assert_eq!(to_progress_response(&moving).title, "Moving 2 of 3");
        assert_eq!(
            preparing_response(dto::Direction::Download, true).title,
            "Preparing move…"
        );
    }

    #[test]
    fn removing_shows_how_many_originals_are_gone() {
        let removing = dto::TransferProgress {
            moving: true,
            removing: true,
            files_done: 1_200,
            files_total: 4_800,
            ..Default::default()
        };

        let response = to_progress_response(&removing);

        assert_eq!(response.title, "Removing the originals from the phone");
        assert_eq!(response.current_file, "1,200 of 4,800 removed");
        assert!((response.total_fraction - 0.25).abs() < 1e-6);
        assert_eq!(response.note, None);
    }

    // ---- inside the phone, through the Mac ----

    fn through_mac(files: usize, removal: Option<dto::Removal>) -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Upload,
            files,
            bytes: 2_500_000,
            destination: "/DCIM/Camera".into(),
            first_item: Some("a (1).jpg".into()),
            removal,
            through_mac: true,
            ..Default::default()
        }
    }

    #[test]
    fn progress_through_the_mac_says_which_half_and_whether_it_is_a_move() {
        let mut report = progress(1_000_000, 1);
        report.through_mac = true;

        report.direction = dto::Direction::Download;
        assert_eq!(
            to_progress_response(&report).title,
            "Copying via your Mac, reading 2 of 3"
        );
        report.direction = dto::Direction::Upload;
        report.moving = true;
        assert_eq!(
            to_progress_response(&report).title,
            "Moving via your Mac, sending 2 of 3"
        );
        assert_eq!(
            preparing_via_mac(false).title,
            "Preparing copy via your Mac…"
        );
        assert_eq!(
            preparing_via_mac(true).title,
            "Preparing move via your Mac…"
        );
    }

    #[test]
    fn a_copy_through_the_mac_says_copied_and_where() {
        let response = to_summary_response(&through_mac(3, None));

        assert_eq!(response.title, "Copied 3 files (2.5 MB)");
        assert_eq!(
            response.detail,
            "to Camera on the phone, by way of your Mac"
        );
        assert_eq!(
            response.reveal, None,
            "Finder has nothing to show: it stayed on the phone"
        );
    }

    #[test]
    fn a_move_through_the_mac_that_took_everything_says_moved() {
        let response = to_summary_response(&through_mac(
            3,
            Some(dto::Removal {
                removed: 3,
                ..Default::default()
            }),
        ));

        assert_eq!(response.title, "Moved 3 files (2.5 MB)");
        assert!(response.left.is_empty());
    }

    #[test]
    fn a_move_through_the_mac_that_could_not_confirm_a_copy_says_what_stayed() {
        let response = to_summary_response(&through_mac(
            2,
            Some(dto::Removal {
                kept: vec![dto::Kept {
                    label: "Trip".into(),
                    why: dto::Why::Unchecked,
                }],
                left: vec!["Trip".into()],
                ..Default::default()
            }),
        ));

        assert_eq!(response.title, "Copied 2 files (2.5 MB)");
        assert_eq!(
            response.notes,
            ["Trip — stayed on the phone: its copy couldn't be checked"]
        );
        assert_eq!(response.left, ["Trip"]);
    }

    #[test]
    fn a_move_into_the_folder_it_is_in_says_so_and_a_copy_never_looks_empty() {
        let mut nothing = through_mac(0, Some(dto::Removal::default()));
        nothing.first_item = None;

        assert_eq!(
            to_summary_response(&nothing).title,
            "Already in this folder"
        );
    }

    #[test]
    fn a_failure_through_the_mac_still_says_nothing_was_copied() {
        let mut summary = through_mac(0, None);
        summary.failures = vec![dto::FileFailure {
            label: "a".into(),
            error: dto::Error::ReadOnly,
        }];

        assert_eq!(to_summary_response(&summary).title, "Nothing was copied");
    }

    #[test]
    fn the_queue_names_a_copy_through_the_mac() {
        assert_eq!(
            on_phone_label(dto::Paste::Copy, &["IMG_1.jpg".into()], "/DCIM/Camera"),
            "Copy “IMG_1.jpg” to Camera via your Mac"
        );
        assert_eq!(
            on_phone_label(dto::Paste::Move, &["a".into(), "b".into()], "/"),
            "Move 2 items to the top folder via your Mac"
        );
    }

    fn imported(files: usize, already_there: usize) -> dto::TransferSummary {
        dto::TransferSummary {
            files,
            bytes: files as u64 * 1_000_000,
            already_there,
            destination: "/Users/me/Pictures".into(),
            ..Default::default()
        }
    }

    #[test]
    fn an_import_says_what_it_brought_and_what_was_there() {
        let many = to_import_summary(&imported(1_234, 50));
        assert_eq!(many.title, "Imported 1,234 photos and videos (1.2 GB)");
        assert_eq!(many.detail, "to Pictures · 50 imported before");
        assert_eq!(
            to_import_summary(&imported(1, 0)).title,
            "Imported 1 photo or video (1.0 MB)"
        );
        // A retry that found everything arrived.
        assert_eq!(
            to_import_summary(&imported(0, 50)).title,
            "No new photos or videos"
        );
        let failed = dto::TransferSummary {
            failures: vec![dto::FileFailure {
                label: "a.jpg".into(),
                error: dto::Error::Unsupported,
            }],
            ..imported(0, 0)
        };
        assert_eq!(to_import_summary(&failed).title, "Nothing was copied");
    }

    #[test]
    fn an_import_that_freed_the_phone_says_how_much_and_what_stayed() {
        let freed = dto::TransferSummary {
            removal: Some(dto::Removal {
                removed: 3,
                freed: 12_400_000_000,
                kept: vec![dto::Kept {
                    label: "DCIM/Camera/b.mp4".into(),
                    why: dto::Why::Unchecked,
                }],
                ..dto::Removal::default()
            }),
            ..imported(4, 0)
        };

        let response = to_import_summary(&freed);

        assert_eq!(response.title, "Imported 4 photos and videos (4.0 MB)");
        assert_eq!(
            response.detail,
            "to Pictures · 3 files removed from the phone, 12.4 GB freed"
        );
        assert_eq!(
            response.notes,
            ["DCIM/Camera/b.mp4 — stayed on the phone: its copy couldn't be checked"]
        );
        let with_failure = dto::TransferSummary {
            failures: vec![dto::FileFailure {
                label: "a.jpg".into(),
                error: dto::Error::Unsupported,
            }],
            ..freed
        };
        assert!(
            !to_import_summary(&with_failure).title.starts_with("Moved"),
            "an import is never called a move"
        );
        let nothing = dto::TransferSummary {
            removal: Some(dto::Removal::default()),
            ..imported(0, 2)
        };
        assert_eq!(
            to_import_summary(&nothing).detail,
            "to Pictures · 2 imported before"
        );
    }

    #[test]
    fn an_import_in_progress_is_importing() {
        let shown = to_import_progress(&progress(500_000, 1));
        assert!(
            shown.title.starts_with("Importing 2 of 3"),
            "{}",
            shown.title
        );
        assert_eq!(
            import_label("/Users/me/Pictures"),
            "Import new photos to Pictures"
        );
        assert_eq!(preparing_import().title, "Looking for new photos…");
    }

    fn mirrored(
        direction: dto::Direction,
        files: usize,
        mirrored: dto::Mirrored,
    ) -> dto::TransferSummary {
        dto::TransferSummary {
            direction,
            files,
            bytes: files as u64,
            destination: "/Backup".into(),
            mirrored: Some(mirrored),
            ..Default::default()
        }
    }

    #[test]
    fn a_mirror_says_what_it_copied_replaced_removed_and_left() {
        let nothing = to_mirror_summary(&mirrored(
            dto::Direction::Download,
            0,
            dto::Mirrored {
                kept: 4,
                ..Default::default()
            },
        ));
        assert_eq!(nothing.title, "Already the same");
        assert_eq!(nothing.detail, "into Backup · 4 only there, kept");

        let onto_phone = to_mirror_summary(&mirrored(
            dto::Direction::Upload,
            1,
            dto::Mirrored {
                replaced: 1,
                removed: 2,
                ..Default::default()
            },
        ));
        assert_eq!(
            onto_phone.title,
            "Mirrored: 1 file replaced, 2 items removed"
        );
        assert_eq!(
            onto_phone.detail,
            "onto Backup · removed ones deleted from the phone"
        );
        let copied = to_mirror_summary(&mirrored(
            dto::Direction::Download,
            4,
            dto::Mirrored {
                copied: 3,
                ..Default::default()
            },
        ));
        assert_eq!(
            copied.title, "Mirrored: 3 files copied",
            "from what was copied, not worked out from the files"
        );

        let replaced_on_mac = to_mirror_summary(&mirrored(
            dto::Direction::Download,
            1,
            dto::Mirrored {
                replaced: 1,
                ..Default::default()
            },
        ));
        assert_eq!(
            replaced_on_mac.detail,
            "into Backup · the old versions are in the Trash"
        );

        let failed = dto::TransferSummary {
            failures: vec![dto::FileFailure {
                label: "a.jpg".into(),
                error: dto::Error::Unsupported,
            }],
            ..mirrored(dto::Direction::Download, 0, dto::Mirrored::default())
        };
        let failed = to_mirror_summary(&failed);
        assert_eq!(failed.title, "Nothing was mirrored");
        assert_eq!(failed.detail, "into Backup · 1 failed");
    }

    #[test]
    fn a_mirror_in_progress_is_mirroring_either_way() {
        let down = to_mirror_progress(&progress(500_000, 1));
        let up = to_mirror_progress(&dto::TransferProgress {
            direction: dto::Direction::Upload,
            ..progress(500_000, 1)
        });
        assert!(down.title.starts_with("Mirroring 2 of 3"), "{}", down.title);
        assert!(up.title.starts_with("Mirroring 2 of 3"), "{}", up.title);
        assert_eq!(preparing_mirror().title, "Comparing the folders…");
        let request = dto::MirrorRequest {
            storage_id: 1,
            phone_folder: "/DCIM/Camera".into(),
            mac_folder: "/Users/me/Photos".into(),
            direction: dto::MirrorDirection::ToMac,
            remove_extras: false,
        };
        assert_eq!(mirror_label(&request), "Mirror Camera onto the Mac");
    }

    #[test]
    fn a_name_from_the_phone_or_the_mac_is_shown_exactly_in_every_language() {
        let odd_names = ["รูปทริป", "写真.jpg", "{x} {{y}}", "{count} {{", "}"];

        for language in dto::Language::ALL {
            crate::i18n::with_language(language, || {
                for name in odd_names {
                    let names = vec![name.to_string()];
                    let folder = format!("/Users/me/{name}");
                    let messages = [
                        job_label(
                            dto::Direction::Download,
                            false,
                            &[format!("/DCIM/{name}")],
                            &folder,
                        ),
                        job_label(
                            dto::Direction::Upload,
                            true,
                            std::slice::from_ref(&folder),
                            &folder,
                        ),
                        on_phone_label(dto::Paste::Copy, &names, &folder),
                        conflict_title(&names, true),
                        conflict_title(&names, false),
                        delete_keeping_label(&names),
                        send_edit_label(name),
                        edit_saved_title(name),
                        edit_open_title(name),
                        import_label(&folder),
                        paused_title(name),
                        waiting_title(name),
                        saved_note(&format!("{folder}/report.tsv")),
                        to_edit_summary(&dto::TransferSummary::default(), name).title,
                    ];
                    for message in messages {
                        assert!(
                            message.contains(name),
                            "{language:?}: {message:?} should show {name:?} exactly"
                        );
                    }
                }
            });
        }
    }
}
