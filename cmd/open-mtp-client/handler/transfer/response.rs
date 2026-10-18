//! Display-ready view models: every string here is shown as-is.

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressResponse {
    /// "Downloading 2 of 12"
    pub title: String,
    pub current_file: String,
    /// 0.0 to 1.0
    pub file_fraction: f32,
    /// 0.0 to 1.0
    pub total_fraction: f32,
    /// "12.3 MB of 48.0 MB · 5.2 MB/s · about 8 s left"
    pub detail: String,
    /// Said under the bars when it helps: a long copy needs the phone to stay
    /// unlocked and plugged in.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SummaryResponse {
    /// "Downloaded 12 files (48.0 MB)"
    pub title: String,
    /// "to Downloads · 1 saved under a new name"
    pub detail: String,
    /// What "Show in Finder" selects.
    pub reveal: Option<String>,
    /// What could not be copied while the rest went on, one line each ("IMG_1.jpg —
    /// The phone refused…"), the first few only. Empty when everything went.
    pub failures: Vec<String>,
    /// What a move left on the phone and why, one line each ("IMG_2.jpg —
    /// stayed on the phone: it changed after it was copied"), and how removing
    /// ended if it stopped early. Empty for a copy and for a clean move.
    pub notes: Vec<String>,
    /// The selected items a move has not finished with: what "Retry" moves.
    pub left: Vec<String>,
    /// The selection counted at the end: "142 files, 3.2 GB — all arrived".
    pub arrival: Option<String>,
    /// Selected items that were put under a new name because theirs was taken
    /// (what a retry must not mistake for the folder the first try started).
    pub renamed: usize,
    /// What became of each file, for "Details" (downloads, moves to the Mac
    /// and imports; empty for the rest).
    pub report: Vec<ReportRow>,
    /// Files past the most a report lists.
    pub report_left_out: usize,
    /// The Mac folder "Save Report" writes into: where the files went.
    pub report_folder: Option<String>,
    /// How long it took and how fast: "Took 1 min 11 s · 45.2 MB/s on
    /// average"; `None` for what took under a second.
    pub timing: Option<String>,
    /// A check of contents found files that are not the same on both sides
    /// (they differ, or are on one side only): done, but not good news.
    pub mismatched: bool,
}

/// One line of a transfer's report, worded once when the transfer ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    /// "✓", "=", "↻", "–" or "✕".
    pub mark: &'static str,
    /// As the file is named where it went, relative to that folder.
    pub path: String,
    /// "Copied and checked", "Failed: The phone stopped answering"…
    pub what: String,
    /// For a move: "Original removed", "Original kept: it changed…".
    pub original: String,
}
