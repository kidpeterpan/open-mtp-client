use std::path::PathBuf;

/// A panic the app caught as it happened, or a crash report left by an earlier
/// run. What is in it is only where in the code it happened and when: the
/// report's own text can hold names from the user's files, so it stays in its
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashNote {
    /// Milliseconds since 1970-01-01 UTC.
    pub at_ms: u64,
    /// Where in the app's code: "internal/service/transfer/service.rs:120:9".
    /// Never a path on this Mac.
    pub location: String,
    /// The user has been told about it (the file was renamed `reported-…`).
    pub reported: bool,
    /// The report's file; `None` when it could not be written.
    pub file: Option<PathBuf>,
}
